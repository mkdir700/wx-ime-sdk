//! Clipboard exchange: publishing text/images, polling for received content and the
//! types that describe it.
pub(crate) mod cos;
mod receive;
mod send;
mod types;

pub(crate) use receive::ReceiveState;
pub use types::{
    ClipboardContent, ClipboardInfo, DownloadedImage, Event, ReceivedClipboard, ReceivedId,
    SendReceipt,
};
