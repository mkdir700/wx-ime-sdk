#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
mod capture;
mod client;
mod clipboard;
mod cos;
mod device;
mod error;
mod events;
#[doc(hidden)]
pub mod file_identity;
mod file_receiver;
#[doc(hidden)]
pub mod file_rendezvous;
#[doc(hidden)]
pub mod file_transport;
mod files;
mod invite;
mod pairing;
mod receive;
mod session;
mod storage;

pub use client::{Client, ClientOptions};
pub use device::{Device, Group, LocalDevice};
pub use error::{Error, Result};
pub use events::{
    ClipboardContent, DownloadedImage, Event, ReceivedClipboard, ReceivedId, SendReceipt,
};
pub use file_receiver::FileReceiver;
pub use file_rendezvous::FileCode;
pub use file_transport::{FileReceipt, ReceivedFile};
pub use files::{FileReceiveOptions, FileTransfer, FileTransferProgress};
pub use invite::FileInvite;
pub use pairing::{PairingPeer, PairingSession, PairingUpdate};
pub use wx_ime_protocol::pairing::PairingCode;
