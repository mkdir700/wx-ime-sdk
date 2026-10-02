//! Native gRPC transport. Application rendezvous is added by the SDK control layer.
//! One upload and one download are active at a time; completion requires range ACKs.
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::{
    fs::File,
    io::AsyncReadExt,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_stream::{
    StreamExt,
    wrappers::{ReceiverStream, TcpListenerStream},
};
use tonic::{
    Request, Response, Status, Streaming,
    transport::{Certificate, ClientTlsConfig, Endpoint, Identity, Server, ServerTlsConfig},
};
use wx_ime_protocol::flurry::{Action, FileChunk, FileInfo};

mod download;
mod task;
mod upload;
use download::Incoming;
pub use task::FileTask;
use task::{TaskState, task};

mod grpc {
    include!(concat!(env!("OUT_DIR"), "/flurry.rs"));
}
const BLOCK: usize = 2 * 1024 * 1024;
const MAX_CONTROL: usize = 64 * 1024;
const MAX_FILE: u64 = 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("file transport closed before completion")]
    Closed,
    #[error("invalid file transfer: {0}")]
    Invalid(&'static str),
    #[error("file transfer IO failed")]
    Io(#[from] std::io::Error),
    #[error("file transport TLS or gRPC failed")]
    Transport,
    #[error("unverified file action {0}")]
    UnknownAction(u32),
    #[error("file task {id} canceled")]
    Canceled { id: String },
    #[error("file transfer failed: {0}")]
    Failed(Arc<FileError>),
}
type Result<T> = std::result::Result<T, FileError>;

/// PEM material must come from the authenticated rendezvous transaction.
/// Deliberately has no Debug implementation.
pub struct ServerCredentials {
    pub certificate: Vec<u8>,
    pub private_key: zeroize::Zeroizing<Vec<u8>>,
    pub peer_ca: Vec<u8>,
}
/// Certificate/name received through authenticated peer rendezvous.
pub struct ClientCredentials {
    pub certificate: Vec<u8>,
    pub private_key: zeroize::Zeroizing<Vec<u8>>,
    pub peer_ca: Vec<u8>,
    pub server_name: String,
}
#[derive(Debug)]
pub struct FileReceipt {
    pub id: String,
    pub bytes: u64,
}
pub type FileUpload = FileTask<FileReceipt>;
pub type FileDownload = FileTask<ReceivedFile>;
#[derive(Debug)]
pub struct ReceivedFile {
    pub id: String,
    pub path: PathBuf,
    pub bytes: u64,
}
struct UploadCommand {
    file: File,
    info: FileInfo,
    permit: tokio::sync::OwnedSemaphorePermit,
    reply: oneshot::Sender<Result<FileUpload>>,
}

struct ControlCommand {
    wire_id: u32,
    payload: Vec<u8>,
    reply: oneshot::Sender<Result<()>>,
}
/// Raw native business message. Unknown native metadata is preserved.
pub struct FileMessage {
    pub payload: Vec<u8>,
    pub field7: Option<u64>,
    pub field8: Option<Vec<u8>>,
}
impl std::fmt::Debug for FileMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileMessage")
            .field("bytes", &self.payload.len())
            .finish_non_exhaustive()
    }
}
pub struct FileConnection {
    control: mpsc::Sender<ControlCommand>,
    messages: tokio::sync::Mutex<mpsc::Receiver<FileMessage>>,
    wire_id: AtomicU32,
    commands: mpsc::Sender<UploadCommand>,
    upload_slots: Arc<tokio::sync::Semaphore>,
    received: tokio::sync::Mutex<mpsc::Receiver<FileDownload>>,
    worker: JoinHandle<()>,
    terminal: watch::Receiver<Option<Arc<FileError>>>,
    lifetime: tokio_util::sync::CancellationToken,
    server: Option<JoinHandle<()>>,
}
impl FileConnection {
    pub async fn connect(
        address: SocketAddr,
        credentials: ClientCredentials,
        download_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::connect_scoped(
            address,
            credentials,
            download_dir,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    }
    pub(crate) async fn connect_scoped(
        address: SocketAddr,
        credentials: ClientCredentials,
        download_dir: impl AsRef<Path>,
        lifetime: tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(download_dir.as_ref()).await?;
        let dir = tokio::fs::canonicalize(download_dir.as_ref()).await?;
        let tls = ClientTlsConfig::new()
            .identity(Identity::from_pem(
                credentials.certificate,
                credentials.private_key.as_slice(),
            ))
            .ca_certificate(Certificate::from_pem(credentials.peer_ca))
            .domain_name(credentials.server_name);
        let channel = Endpoint::from_shared(format!("https://{address}"))
            .map_err(|_| FileError::Transport)?
            .connect_timeout(std::time::Duration::from_secs(10))
            .tls_config(tls)
            .map_err(|_| FileError::Transport)?
            .connect()
            .await
            .map_err(|_| FileError::Transport)?;
        let mut client = grpc::file_service_client::FileServiceClient::new(channel)
            .max_decoding_message_size(BLOCK + 4096)
            .max_encoding_message_size(BLOCK + 4096);
        let (send, output) = mpsc::channel(2);
        emit(&send, message(100, None)).await?;
        let response = client
            .transfer_files(ReceiverStream::new(output).filter_map(|value| value.ok()))
            .await
            .map_err(|_| FileError::Transport)?;
        Ok(start_worker(
            response.into_inner(),
            send,
            dir,
            true,
            lifetime,
        ))
    }

    /// Returns after the peer acknowledges the native business message.
    pub async fn send_message(&self, payload: &[u8]) -> Result<()> {
        if payload.len() > MAX_CONTROL {
            return Err(FileError::Invalid("control payload limit"));
        }
        let wire_id = self
            .wire_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                id.checked_add(1).filter(|id| *id <= i32::MAX as u32)
            })
            .map_err(|_| FileError::Invalid("control id exhausted"))?;
        let (reply, result) = oneshot::channel();
        if self
            .control
            .send(ControlCommand {
                wire_id,
                payload: payload.to_vec(),
                reply,
            })
            .await
            .is_err()
        {
            return Err(self.failure().await);
        }
        match result.await {
            Ok(value) => value,
            Err(_) => Err(self.failure().await),
        }
    }
    pub async fn next_message(&self) -> Result<FileMessage> {
        match self.messages.lock().await.recv().await {
            Some(message) => Ok(message),
            None => Err(self.failure().await),
        }
    }
    async fn failure(&self) -> FileError {
        task::failure(self.terminal.clone()).await
    }
    pub async fn send_file(&self, path: impl AsRef<Path>) -> Result<FileReceipt> {
        self.start_file(path).await?.wait().await
    }
    pub async fn start_file(&self, path: impl AsRef<Path>) -> Result<FileUpload> {
        let prepare = async {
            let permit = self
                .upload_slots
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| FileError::Closed)?;
            let (file, info) = upload::snapshot(path.as_ref()).await?;
            let (reply, result) = oneshot::channel();
            if self
                .commands
                .send(UploadCommand {
                    file,
                    info,
                    permit,
                    reply,
                })
                .await
                .is_err()
            {
                return Err(self.failure().await);
            }
            match result.await {
                Ok(value) => value,
                Err(_) => Err(self.failure().await),
            }
        };
        tokio::select! {
            biased;
            failure = self.failure() => Err(failure),
            result = prepare => result,
        }
    }
    pub async fn next_file(&self) -> Result<ReceivedFile> {
        self.next_download().await?.wait().await
    }
    pub async fn next_download(&self) -> Result<FileDownload> {
        match self.received.lock().await.recv().await {
            Some(file) => Ok(file),
            None => Err(self.failure().await),
        }
    }
}
impl Drop for FileConnection {
    fn drop(&mut self) {
        self.worker.abort();
        self.lifetime.cancel();
        if let Some(server) = &self.server {
            server.abort();
        }
    }
}

pub struct FileListener {
    lifetime: tokio_util::sync::CancellationToken,
    address: SocketAddr,
    accepted: oneshot::Receiver<FileConnection>,
    server: Option<JoinHandle<()>>,
}
impl FileListener {
    pub async fn bind(
        address: SocketAddr,
        credentials: ServerCredentials,
        download_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let listener = tokio::net::TcpListener::bind(address).await?;
        Self::from_listener(listener, credentials, download_dir).await
    }
    pub(crate) async fn from_listener(
        listener: tokio::net::TcpListener,
        credentials: ServerCredentials,
        download_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::from_listener_scoped(
            listener,
            credentials,
            download_dir,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
    }
    pub(crate) async fn from_listener_scoped(
        listener: tokio::net::TcpListener,
        credentials: ServerCredentials,
        download_dir: impl AsRef<Path>,
        lifetime: tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        let address = listener.local_addr()?;
        tokio::fs::create_dir_all(download_dir.as_ref()).await?;
        let download_dir = tokio::fs::canonicalize(download_dir.as_ref()).await?;
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(
                credentials.certificate,
                credentials.private_key.as_slice(),
            ))
            .client_ca_root(Certificate::from_pem(credentials.peer_ca));
        let (accepted, receiver) = oneshot::channel();
        let service = Service {
            accepted: Arc::new(Mutex::new(Some(accepted))),
            download_dir,
            lifetime: lifetime.clone(),
        };
        let mut server = Server::builder()
            .tls_config(tls)
            .map_err(|_| FileError::Transport)?;
        let shutdown = lifetime.clone();
        let task = tokio::spawn(async move {
            tokio::select! {
                _ = server.add_service(grpc::file_service_server::FileServiceServer::new(service)
                    .max_decoding_message_size(BLOCK + 4096).max_encoding_message_size(BLOCK + 4096))
                    .serve_with_incoming(TcpListenerStream::new(listener)) => {},
                () = shutdown.cancelled() => {},
            }
        });
        Ok(Self {
            lifetime,
            address,
            accepted: receiver,
            server: Some(task),
        })
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub async fn accept(mut self) -> Result<FileConnection> {
        let mut connection = (&mut self.accepted).await.map_err(|_| FileError::Closed)?;
        connection.server = self.server.take();
        Ok(connection)
    }
}
impl Drop for FileListener {
    fn drop(&mut self) {
        if let Some(server) = &self.server {
            self.lifetime.cancel();
            server.abort();
        }
    }
}
#[derive(Clone)]
struct Service {
    lifetime: tokio_util::sync::CancellationToken,
    accepted: Arc<Mutex<Option<oneshot::Sender<FileConnection>>>>,
    download_dir: PathBuf,
}
#[tonic::async_trait]
impl grpc::file_service_server::FileService for Service {
    type TransferFilesStream = ReceiverStream<std::result::Result<FileChunk, Status>>;
    async fn transfer_files(
        &self,
        request: Request<Streaming<FileChunk>>,
    ) -> std::result::Result<Response<Self::TransferFilesStream>, Status> {
        let accepted = self
            .accepted
            .lock()
            .map_err(|_| Status::internal("accept state"))?
            .take()
            .ok_or_else(|| Status::resource_exhausted("one active file connection"))?;
        let (send, stream) = mpsc::channel(2);
        if self.lifetime.is_cancelled() {
            return Err(Status::cancelled("file receiver closed"));
        }
        let connection = start_worker(
            request.into_inner(),
            send,
            self.download_dir.clone(),
            false,
            self.lifetime.clone(),
        );
        accepted
            .send(connection)
            .map_err(|_| Status::cancelled("listener dropped"))?;
        Ok(Response::new(ReceiverStream::new(stream)))
    }
}
fn start_worker(
    stream: Streaming<FileChunk>,
    send: mpsc::Sender<std::result::Result<FileChunk, Status>>,
    dir: PathBuf,
    initialized: bool,
    lifetime: tokio_util::sync::CancellationToken,
) -> FileConnection {
    let (commands, receive_commands) = mpsc::channel(1);
    let (control, receive_control) = mpsc::channel(1);
    let (messages, received_messages) = mpsc::channel(16);
    let (received, files) = mpsc::channel(1);
    let (failed, terminal) = watch::channel(None);
    let worker_terminal = terminal.clone();
    let stopped = lifetime.clone();
    let worker = tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            () = stopped.cancelled() => Err(FileError::Closed),
            result = transfer(stream, &send, TransferIo {commands:receive_commands,controls:receive_control,received,messages,terminal:worker_terminal},dir,initialized) => result,
        };
        if let Err(error) = result {
            let error = Arc::new(error);
            failed.send_replace(Some(error.clone()));
            if !stopped.is_cancelled() {
                let _ = send
                    .send(Err(Status::failed_precondition(error.to_string())))
                    .await;
            }
        }
    });
    FileConnection {
        control,
        messages: tokio::sync::Mutex::new(received_messages),
        wire_id: AtomicU32::new(1),
        commands,
        upload_slots: Arc::new(tokio::sync::Semaphore::new(2)),
        received: tokio::sync::Mutex::new(files),
        worker,
        terminal,
        lifetime,
        server: None,
    }
}
struct Outgoing {
    file: File,
    info: FileInfo,
    offset: u64,
    awaiting: Option<(u64, u64)>,
    state: TaskState<FileReceipt>,
    sequence: u64,
    _permit: tokio::sync::OwnedSemaphorePermit,
}
fn id(info: &FileInfo) -> Result<&str> {
    let id = info
        .id
        .as_deref()
        .ok_or(FileError::Invalid("missing file id"))?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(FileError::Invalid("file id"));
    }
    Ok(id)
}
fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 240
        || value == "."
        || value == ".."
        || value.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0'))
    {
        return Err(FileError::Invalid("file name"));
    }
    Ok(())
}
fn message(kind: u32, info: Option<FileInfo>) -> FileChunk {
    FileChunk {
        action: Some(Action {
            kind: Some(kind),
            file: info,
        }),
        ..Default::default()
    }
}
async fn emit(
    sender: &mpsc::Sender<std::result::Result<FileChunk, Status>>,
    chunk: FileChunk,
) -> Result<()> {
    sender.send(Ok(chunk)).await.map_err(|_| FileError::Closed)
}
struct TransferIo {
    commands: mpsc::Receiver<UploadCommand>,
    controls: mpsc::Receiver<ControlCommand>,
    received: mpsc::Sender<FileDownload>,
    messages: mpsc::Sender<FileMessage>,
    terminal: watch::Receiver<Option<Arc<FileError>>>,
}
async fn transfer(
    mut stream: Streaming<FileChunk>,
    sender: &mpsc::Sender<std::result::Result<FileChunk, Status>>,
    io: TransferIo,
    dir: PathBuf,
    mut initialized: bool,
) -> Result<()> {
    let TransferIo {
        mut commands,
        mut controls,
        received,
        messages,
        terminal,
    } = io;
    let mut pending_control: Option<(Vec<u8>, oneshot::Sender<Result<()>>)> = None;
    let mut control_sequence = 0u64;
    let mut file_sequence = 0u64;
    let mut canceled_upload: Option<(String, u64)> = None;
    let mut upload: Option<Outgoing> = None;
    let mut pending_upload: Option<UploadCommand> = None;
    let mut download: Option<Incoming> = None;
    let mut canceled_download: Option<String> = None;
    loop {
        let has_pending_upload = pending_upload.is_some();
        let can_start_upload = initialized && upload.is_none() && has_pending_upload;
        tokio::select! {
            incoming = stream.message() => {
                let chunk = incoming.map_err(|_| FileError::Transport)?.ok_or(FileError::Closed)?;
                let action = chunk.action.as_ref().ok_or(FileError::Invalid("missing action"))?;
                let kind = action.kind.ok_or(FileError::Invalid("missing action kind"))?;
                if !initialized {
                    if kind != 100 { return Err(FileError::Invalid("missing stream initialization")); }
                    initialized = true;
                    continue;
                }
                match kind {
                    1 => {
                        if download.is_some() { return Err(FileError::Invalid("overlapping file metadata")); }
                        let info = action.file.clone().ok_or(FileError::Invalid("missing file metadata"))?;
                        id(&info)?;
                        name(info.name.as_deref().ok_or(FileError::Invalid("missing file name"))?)?;
                        if info.size.ok_or(FileError::Invalid("missing file size"))? > MAX_FILE { return Err(FileError::Invalid("file size")); }
                        // A slow host must not block controls and unrelated IO.
                        // Reject only this task using the established receiver
                        // cancellation action, before allocating temporary data.
                        let slot = match received.try_reserve() {
                            Ok(slot) => slot,
                            Err(mpsc::error::TrySendError::Full(())) => {
                                let file_id = id(&info)?.to_owned();
                                canceled_download = Some(file_id.clone());
                                emit(sender, message(4, Some(FileInfo { id: Some(file_id), ..Default::default() }))).await?;
                                continue;
                            }
                            Err(mpsc::error::TrySendError::Closed(())) => return Err(FileError::Closed),
                        };
                        let (incoming, handle) = Incoming::new(&dir, info, terminal.clone())?;
                        canceled_download = None;
                        download = Some(incoming);
                        slot.send(handle);
                    }
                    2 => {
                        if let Some(canceled) = &canceled_download {
                            let info = action.file.as_ref().ok_or(FileError::Invalid("missing data file"))?;
                            if id(info)? == canceled { continue; }
                        }
                        let active = download.as_mut().ok_or(FileError::Invalid("data before metadata"))?;
                        let (start, end) = active.append(&chunk).await?;
                        let file_id = id(&active.info)?.to_owned();
                        let completed = if end == active.info.size.unwrap_or(0) {
                            Some(download.take().expect("active download").commit(&dir).await?)
                        } else { None };
                        let mut ack = message(5, Some(FileInfo { id: Some(file_id), ..Default::default() }));
                        ack.start = Some(start); ack.end = Some(end);
                        emit(sender, ack).await?;
                        if let Some((file, completion)) = completed { let _ = completion.send(Ok(file)); }
                    }
                    3 => {
                        let info = action.file.as_ref().ok_or(FileError::Invalid("missing cancel file"))?;
                        let file_id = id(info)?.to_owned();
                        let active = download.as_ref().ok_or(FileError::Invalid("unexpected upload cancel"))?;
                        if id(&active.info)? != file_id { return Err(FileError::Invalid("foreign upload cancel")); }
                        let active = download.take().expect("validated download");
                        canceled_download = Some(file_id.clone());
                        active.cancel(file_id);
                    }
                    4 => {
                        let info = action.file.as_ref().ok_or(FileError::Invalid("missing cancel file"))?;
                        let file_id = id(info)?.to_owned();
                        let active = upload.as_ref().ok_or(FileError::Invalid("unexpected download cancel"))?;
                        if id(&active.info)? != file_id { return Err(FileError::Invalid("foreign download cancel")); }
                        let active = upload.take().expect("validated upload");
                        canceled_upload = Some((file_id.clone(), active.offset));
                        file_sequence = active.sequence.checked_add(1).ok_or(FileError::Invalid("file sequence exhausted"))?;
                        let _ = active.state.completion.send(Err(FileError::Canceled { id: file_id }));
                    }
                    5 if action.file.is_none() => {
                        let metadata = chunk.field8.as_ref().ok_or(FileError::Invalid("missing control ACK id"))?;
                        let (expected, reply) = pending_control.take().ok_or(FileError::Invalid("unexpected control ACK"))?;
                        if metadata != &expected { return Err(FileError::Invalid("foreign control ACK")); }
                        let _ = reply.send(Ok(()));
                    }
                    5 => {
                        let ack_info = action.file.as_ref().ok_or(FileError::Invalid("missing ACK file"))?;
                        if let Some((canceled, limit)) = &canceled_upload
                            && id(ack_info)? == canceled && upload.as_ref().is_none_or(|active| active.info.id.as_deref() != Some(canceled)) {
                            if chunk.end.is_none_or(|end| end > *limit) { return Err(FileError::Invalid("late cancel ACK range")); }
                            continue;
                        }
                        let active = upload.as_mut().ok_or(FileError::Invalid("unexpected file ACK"))?;
                        let info = action.file.as_ref().ok_or(FileError::Invalid("missing ACK file"))?;
                        if id(info)? != id(&active.info)? || active.awaiting != Some((chunk.start.ok_or(FileError::Invalid("missing ACK start"))?,chunk.end.ok_or(FileError::Invalid("missing ACK end"))?)) {
                            return Err(FileError::Invalid("foreign or invalid ACK"));
                        }
                        active.awaiting = None;
                        active.state.progress.send_replace(active.offset);
                        if active.offset == active.info.size.unwrap_or(0) {
                            let active = upload.take().expect("active upload");
                            file_sequence = active.sequence.checked_add(1).ok_or(FileError::Invalid("file sequence exhausted"))?;
                            let _ = active.state.completion.send(Ok(FileReceipt { id: id(&active.info)?.to_owned(), bytes: active.offset }));
                        }
                    }
                    200 => {
                        let payload=chunk.business.ok_or(FileError::Invalid("missing control payload"))?;
                        if payload.len()>MAX_CONTROL||chunk.field8.as_ref().is_some_and(|bytes|bytes.len()>1024){return Err(FileError::Invalid("control payload limit"));}
                        let metadata = chunk.field8.ok_or(FileError::Invalid("missing control id"))?;
                        messages.send(FileMessage{payload,field7:chunk.field7,field8:Some(metadata.clone())}).await.map_err(|_|FileError::Closed)?;
                        let mut ack = message(5, None); ack.field8 = Some(metadata);
                        emit(sender, ack).await?;
                    }
                    _ => return Err(FileError::UnknownAction(kind)),
                }
            }
            () = async { upload.as_ref().expect("active upload").state.cancel.cancelled().await }, if upload.is_some() => {
                let active = upload.take().expect("active upload");
                let file_id = id(&active.info)?.to_owned();
                let mut cancel = message(3, Some(FileInfo { id: Some(file_id.clone()), ..Default::default() }));
                file_sequence = file_sequence.checked_add(1).ok_or(FileError::Invalid("file sequence exhausted"))?;
                cancel.field6 = Some(file_sequence);
                emit(sender, cancel).await?;
                file_sequence = file_sequence.checked_add(1).ok_or(FileError::Invalid("file sequence exhausted"))?;
                canceled_upload = Some((file_id.clone(), active.offset));
                let _ = active.state.completion.send(Err(FileError::Canceled { id: file_id }));
            }
            () = async { download.as_ref().expect("active download").state.cancel.cancelled().await }, if download.is_some() => {
                let active = download.take().expect("active download");
                let file_id = id(&active.info)?.to_owned();
                let cancel = message(4, Some(FileInfo { id: Some(file_id.clone()), ..Default::default() }));
                emit(sender, cancel).await?;
                canceled_download = Some(file_id.clone());
                active.cancel(file_id);
            }
            control=controls.recv(), if initialized && pending_control.is_none() => {
                let control=control.ok_or(FileError::Closed)?;
                if control.reply.is_closed(){continue;}
                let mut chunk=message(200,None);chunk.business=Some(control.payload);chunk.field7=Some(control_sequence);
                control_sequence = control_sequence.checked_add(1).ok_or(FileError::Invalid("control sequence exhausted"))?;
                let mut metadata=Vec::new();prost::encoding::encode_varint(control.wire_id as u64,&mut metadata);chunk.field8=Some(metadata.clone());
                pending_control = Some((metadata, control.reply));
                emit(sender,chunk).await?;
            }
            command = commands.recv(), if initialized && !has_pending_upload => {
                pending_upload = Some(command.ok_or(FileError::Closed)?);
            }
            () = async { pending_upload.as_mut().expect("pending upload").reply.closed().await }, if has_pending_upload => {
                // A canceled caller must not pin its snapshot while another file awaits ACK.
                pending_upload.take();
            }
            () = async {}, if can_start_upload => {
                let command = pending_upload.take().expect("pending upload");
                if command.reply.is_closed(){continue;}
                let UploadCommand {file,info,permit,reply} = command;
                let (handle, state) = task(id(&info)?.to_owned(), info.size.unwrap_or(0), terminal.clone());
                let mut metadata = message(1,Some(info.clone())); metadata.field6 = Some(file_sequence);
                file_sequence = file_sequence.checked_add(1).ok_or(FileError::Invalid("file sequence exhausted"))?;
                let active=Outgoing{file,info,offset:0,awaiting:None,state,sequence:file_sequence,_permit:permit};
                emit(sender,metadata).await?;
                upload = Some(active);
                let _ = reply.send(Ok(handle));
            }
            () = async {}, if upload.as_ref().is_some_and(|a| a.awaiting.is_none()) => {
                let active = upload.as_mut().expect("active upload");
                let size = active.info.size.unwrap_or(0);
                let n = (size-active.offset).min(BLOCK as u64) as usize;
                let mut data = vec![0;n]; active.file.read_exact(&mut data).await?;
                let start = active.offset; active.offset += n as u64;
                let mut chunk = message(2,Some(active.info.clone()));
                chunk.data = Some(data); chunk.start = Some(start); chunk.end = Some(active.offset); chunk.field6 = Some(active.sequence);
                active.awaiting = Some((start,active.offset));
                emit(sender,chunk).await?;
            }
        }
    }
}
