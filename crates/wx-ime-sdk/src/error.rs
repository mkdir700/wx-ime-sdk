#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("file transport operation failed: {0}")]
    File(#[from] crate::file_transport::FileError),
    #[error("operation canceled")]
    Canceled,
    #[error("peer rejected the file transfer")]
    FileRejected,
    #[error("file peer actively disconnected")]
    FilePeerDisconnected,
    #[error("protocol operation failed: {0}")]
    Protocol(#[from] wx_ime_protocol::Error),
    #[error("filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("identity state format invalid")]
    State(#[from] serde_json::Error),
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    #[error("control transport failed")]
    Transport,
    #[error("control HTTP status {0}")]
    Http(u16),
    #[error("command {command} rejected with status {status}")]
    Business { command: u32, status: i32 },
    #[error("client is closed")]
    Closed,
    #[error("identity is already in use")]
    IdentityInUse,
    #[error("pairing requires explicit issuer confirmation")]
    ConfirmationRequired,
    #[error("another pairing session is active")]
    PairingInUse,
    #[error("pairing returned unexpected server state {0}")]
    UnknownBindState(u32),
    #[error("pairing code is no longer available")]
    CodeUnavailable,
    #[error("device is not paired")]
    NotPaired,
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("clipboard publication outcome is unknown; do not retry automatically")]
    OutcomeUnknown,
    #[error("COS transport failed")]
    CosTransport,
    #[error("COS HTTP status {0}")]
    CosHttp(u16),
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn business(command: u32, status: Option<i32>) -> Result<()> {
    match status {
        Some(0) => Ok(()),
        Some(status) => Err(Error::Business { command, status }),
        None => Err(Error::Invalid("missing business status")),
    }
}
