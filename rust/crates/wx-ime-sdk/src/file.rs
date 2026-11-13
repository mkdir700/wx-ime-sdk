//! Plain file transfer between paired devices: invitation discovery, rendezvous through the
//! control service, and the authenticated LAN TLS/gRPC transport.
mod identity;
mod invite;
mod receiver;
mod rendezvous;
mod transfer;
mod transport;

pub use invite::FileInvite;
pub use receiver::FileReceiver;
pub use rendezvous::FileCode;
pub use transfer::{FileReceiveOptions, FileTransfer, FileTransferProgress};
pub use transport::{FileError, FileReceipt, ReceivedFile};
