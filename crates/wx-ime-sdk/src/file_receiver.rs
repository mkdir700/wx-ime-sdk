//! A receiver owns control processing even while its host is idle.
use crate::{
    Error, Result,
    file_transport::{FileConnection, FileError, FileListener, ReceivedFile},
};
use std::{net::SocketAddr, sync::Arc};
use tokio::{sync::watch, task::JoinHandle};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
enum State {
    Waiting,
    Connected(Arc<FileConnection>),
    Failed(Arc<FileError>, Option<Arc<FileConnection>>),
    PeerDisconnected(Arc<FileConnection>),
    Canceled,
}
impl State {
    fn connection(&self) -> Option<Arc<FileConnection>> {
        match self {
            Self::Connected(connection) | Self::PeerDisconnected(connection) => {
                Some(connection.clone())
            }
            Self::Failed(_, connection) => connection.clone(),
            _ => None,
        }
    }
    fn error(&self) -> Option<Error> {
        match self {
            Self::Failed(error, _) => Some(FileError::Failed(error.clone()).into()),
            Self::PeerDisconnected(_) => Some(Error::FilePeerDisconnected),
            Self::Canceled => Some(Error::Canceled),
            _ => None,
        }
    }
}

/// Owns one invitation and connection. Controls continue between receive calls.
/// Drop, cancel and Client shutdown stop acceptance and the connection.
pub struct FileReceiver {
    address: SocketAddr,
    state: watch::Receiver<State>,
    cancel: CancellationToken,
    controls: JoinHandle<()>,
}
impl FileReceiver {
    pub(crate) fn new(
        listener: FileListener,
        cancel: CancellationToken,
        transport: CancellationToken,
    ) -> Self {
        let address = listener.local_addr();
        let (updates, state) = watch::channel(State::Waiting);
        let lifetime = cancel.clone();
        let controls = tokio::spawn(async move {
            let terminal = tokio::select! {
                biased;
                () = lifetime.cancelled() => State::Canceled,
                result = run(listener, &updates) => result,
            };
            updates.send_replace(terminal);
            // Keep the bounded completed-file queue readable after peer close.
            // Stop network resources without marking this as host cancellation.
            transport.cancel();
        });
        Self {
            address,
            state,
            cancel,
            controls,
        }
    }
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub async fn receive(&mut self) -> Result<ReceivedFile> {
        let connection = loop {
            let state = self.state.borrow().clone();
            if self.cancel.is_cancelled() {
                return Err(Error::Canceled);
            }
            if let Some(connection) = state.connection() {
                break connection;
            }
            if let Some(error) = state.error() {
                return Err(error);
            }
            tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Error::Canceled),
                changed = self.state.changed() => changed.map_err(|_| Error::Closed)?,
            }
        };
        let operation = async {
            let cause =
                |error: FileError| self.state.borrow().error().unwrap_or_else(|| error.into());
            let task = connection.next_download().await.map_err(cause)?;
            // Dropping wait requests task cancellation; the scoped connection
            // token also stops the worker on handle cancellation or shutdown.
            task.wait().await.map_err(cause)
        };
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(Error::Canceled),
            result = operation => result,
        }
    }
}
impl Drop for FileReceiver {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.controls.abort();
    }
}
async fn run(listener: FileListener, updates: &watch::Sender<State>) -> State {
    let connection = match listener.accept().await {
        Ok(connection) => Arc::new(connection),
        Err(error) => return State::Failed(Arc::new(error), None),
    };
    updates.send_replace(State::Connected(connection.clone()));
    loop {
        match connection.next_message().await {
            Ok(message) if crate::files::peer_disconnected(&message.payload) => {
                return State::PeerDisconnected(connection);
            }
            Ok(_) => {}
            Err(error) => return State::Failed(Arc::new(error), Some(connection)),
        }
    }
}
