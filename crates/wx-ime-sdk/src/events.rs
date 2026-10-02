use crate::{Device, Group};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Stable receipt identity; acknowledge only after committing content in the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedId {
    pub(crate) group: u64,
    pub(crate) version: u64,
    pub(crate) from_uin: u64,
    pub(crate) trace_id: String,
}
impl ReceivedId {
    pub fn group(&self) -> u64 {
        self.group
    }
    pub fn version(&self) -> u64 {
        self.version
    }
    pub fn from_uin(&self) -> u64 {
        self.from_uin
    }
    pub fn trace_id(&self) -> &str {
        &self.trace_id
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedImage {
    pub path: PathBuf,
    pub bytes: usize,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub enum ClipboardContent {
    Text(String),
    Image(DownloadedImage),
}
impl std::fmt::Debug for ClipboardContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(value) => f.debug_struct("Text").field("bytes", &value.len()).finish(),
            Self::Image(image) => image.fmt(f),
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceivedClipboard {
    pub id: ReceivedId,
    pub source: Device,
    pub content: ClipboardContent,
    pub expire_at: u64,
}
#[derive(Debug, Clone)]
pub enum Event {
    GroupChanged(Group),
    ClipboardReceived(ReceivedClipboard),
}
/// Server acceptance, not evidence of destination clipboard delivery.
#[derive(Debug, Clone, Serialize)]
pub struct SendReceipt {
    pub group: u64,
    pub version: u64,
    pub trace_id: String,
}
