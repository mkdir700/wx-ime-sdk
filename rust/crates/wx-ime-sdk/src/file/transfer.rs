//! Paired-peer file invitations, owned connection lifetime and host-facing tasks.
use crate::{
    Client, Error, FileReceiver, Result,
    file::identity::FileIdentity,
    file::rendezvous::{self, FileCode, FileOffer},
    file::transport::{FileConnection, FileListener, FileReceipt},
};
use prost::Message;
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub enum FileTransferProgress {
    AwaitingPeer,
    Connecting,
    Sending {
        id: String,
        acknowledged_bytes: u64,
        total_bytes: u64,
    },
}
/// One invitation and one file. Drop/cancel stops this operation without retrying it.
pub struct FileTransfer {
    code: FileCode,
    progress: watch::Receiver<FileTransferProgress>,
    result: Option<oneshot::Receiver<Result<FileReceipt>>>,
    cancel: CancellationToken,
}
impl FileTransfer {
    pub fn code(&self) -> &FileCode {
        &self.code
    }
    pub fn progress(&self) -> FileTransferProgress {
        self.progress.borrow().clone()
    }
    pub async fn progress_changed(&mut self) -> bool {
        self.progress.changed().await.is_ok()
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn wait(mut self) -> Result<FileReceipt> {
        self.result
            .take()
            .expect("owned transfer result")
            .await
            .map_err(|_| Error::Closed)?
    }
}
impl Drop for FileTransfer {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Addresses must be reachable by the peer; bind port0 reserves an ephemeral port.
#[derive(Clone, Debug)]
pub struct FileReceiveOptions {
    pub bind_address: SocketAddr,
    pub advertised_ips: Vec<IpAddr>,
    pub download_dir: PathBuf,
}
// The native controller sends permission/lifecycle messages while a file operation
// waits. Drain them alongside file IO so bounded backpressure cannot stall its ACKs.
async fn with_controls<T>(
    connection: &FileConnection,
    operation: impl Future<Output = Result<T>>,
) -> Result<T> {
    let controls = async {
        loop {
            let message = connection.next_message().await?;
            if peer_disconnected(&message.payload) {
                return Err(Error::FilePeerDisconnected);
            }
            // Official E0/R ignores invalid/unknown envelopes.1002-1005 concern
            // optional nearfield upgrades;1100 is only actionable for iOS peers.
        }
    };
    tokio::select! {
        result = operation => result,
        result = controls => result,
    }
}
pub(crate) fn peer_disconnected(payload: &[u8]) -> bool {
    wx_ime_protocol::wire::FileControlEnvelope::decode(payload)
        .is_ok_and(|envelope| envelope.command == Some(1001))
}
impl Client {
    /// Invites an already paired peer, then sends one file over authenticated LAN TLS.
    /// The code is available on the handle for a peer using explicit code acceptance.
    pub async fn send_file(
        &self,
        peer_uin: u64,
        path: impl AsRef<Path>,
        lan: &[IpAddr],
    ) -> Result<FileTransfer> {
        let path = path.as_ref().to_owned();
        let metadata = tokio::fs::metadata(&path).await?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 * 1024 {
            return Err(Error::Invalid("file size or type"));
        }
        validate_ips(lan)?;
        let offer = FileOffer::invite(self, peer_uin, lan).await?;
        let code = offer.code().clone();
        let cancel = self.inner.lifetime.child_token();
        let (updates, progress) = watch::channel(FileTransferProgress::AwaitingPeer);
        let (complete, result) = oneshot::channel();
        let operation_cancel = cancel.clone();
        let client = self.clone();
        tokio::spawn(async move {
            let result = send(&client, offer, path, updates, operation_cancel).await;
            let _ = complete.send(result);
        });
        Ok(FileTransfer {
            code,
            progress,
            result: Some(result),
            cancel,
        })
    }
    /// Accepts a transfer code from the expected paired peer and starts a TLS listener.
    pub async fn receive_file(
        &self,
        peer_uin: u64,
        code: &FileCode,
        options: FileReceiveOptions,
    ) -> Result<FileReceiver> {
        validate_ips(&options.advertised_ips)?;
        let reserved = tokio::net::TcpListener::bind(options.bind_address).await?;
        let port = reserved.local_addr()?.port();
        let addresses: Vec<_> = options
            .advertised_ips
            .iter()
            .map(|ip| SocketAddr::new(*ip, port))
            .collect();
        let identity = FileIdentity::generate().map_err(wx_ime_protocol::Error::from)?;
        let cancel = self.inner.lifetime.child_token();
        let peer = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(Error::Canceled),
            result = rendezvous::join(self, peer_uin, code, &identity, &options.advertised_ips, &addresses) => result?,
        };
        if peer.capabilities & 2 == 0 {
            return Err(Error::Invalid("peer lacks file range ACK capability"));
        }
        let transport = cancel.child_token();
        let listener = FileListener::from_listener_scoped(
            reserved,
            identity.server_credentials(&peer.ca),
            options.download_dir,
            transport.clone(),
        )
        .await?;
        Ok(FileReceiver::new(listener, cancel, transport))
    }
}
fn validate_ips(ips: &[IpAddr]) -> Result<()> {
    if ips.is_empty()
        || ips.len() > 32
        || ips
            .iter()
            .any(|ip| ip.is_unspecified() || ip.is_multicast())
    {
        return Err(Error::Invalid("advertised LAN addresses"));
    }
    Ok(())
}
async fn send(
    client: &Client,
    offer: FileOffer,
    path: PathBuf,
    updates: watch::Sender<FileTransferProgress>,
    cancel: CancellationToken,
) -> Result<FileReceipt> {
    let setup = async {
        loop {
            if let Some(peer) = offer.peer().await?
                && let Some(grpc) = peer.grpc
            {
                if peer.capabilities & 2 == 0 {
                    return Err(Error::Invalid("peer lacks file range ACK capability"));
                }
                updates.send_replace(FileTransferProgress::Connecting);
                if grpc.available_nets.lan.is_empty() || grpc.available_nets.lan.len() > 32 {
                    return Err(Error::Invalid("peer LAN endpoints"));
                }
                for address in grpc.available_nets.lan {
                    if address.port == 0 || address.ip.is_unspecified() || address.ip.is_multicast()
                    {
                        continue;
                    }
                    let cache = client
                        .session()
                        .await?
                        .store
                        .directory
                        .join("file-incoming");
                    if let Ok(connection) = FileConnection::connect(
                        SocketAddr::new(address.ip, address.port),
                        offer
                            .identity()
                            .client_credentials(&peer.ca, &grpc.server_name),
                        cache,
                    )
                    .await
                    {
                        return Ok(connection);
                    }
                }
                return Err(Error::File(crate::file::transport::FileError::Transport));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    };
    let connection: FileConnection = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(Error::Canceled),
        result = tokio::time::timeout(Duration::from_secs(60),setup) => result.map_err(|_|Error::Timeout("file peer connection"))??,
    };
    with_controls(&connection, async {
    let mut task = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(Error::Canceled),
        task = connection.start_file(path) => task?,
    };
    // The owned actor retains the connection until task completion/cancellation.
    loop {
        updates.send_replace(FileTransferProgress::Sending {
            id: task.id().to_owned(),
            acknowledged_bytes: task.bytes_transferred(),
            total_bytes: task.total_bytes(),
        });
        tokio::select! {
            biased;
            () = cancel.cancelled() => { task.cancel(); let _ = task.wait().await; return Err(Error::Canceled); }
            changed = task.progress_changed() => { if !changed { return task.wait().await.map_err(Error::from); } }
        }
    }
    }).await
}
