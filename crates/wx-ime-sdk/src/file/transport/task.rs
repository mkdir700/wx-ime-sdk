//! Task-scoped progress, completion and cancellation ownership.
use super::{FileError, Result};
use std::sync::Arc;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;

/// One transfer task. Dropping the handle requests cancellation of this task only.
pub struct FileTask<T> {
    id: String,
    total: u64,
    progress: watch::Receiver<u64>,
    completion: Option<oneshot::Receiver<Result<T>>>,
    cancel: CancellationToken,
    terminal: watch::Receiver<Option<Arc<FileError>>>,
}
impl<T> FileTask<T> {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn total_bytes(&self) -> u64 {
        self.total
    }
    /// Upload bytes are range-acknowledged; download bytes are written and synced.
    pub fn bytes_transferred(&self) -> u64 {
        *self.progress.borrow()
    }
    pub async fn progress_changed(&mut self) -> bool {
        self.progress.changed().await.is_ok()
    }
    /// Requests local cancellation. Wait for completion to observe the outcome.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
    pub async fn wait(mut self) -> Result<T> {
        match self.completion.take().expect("owned task completion").await {
            Ok(result) => result,
            Err(_) => Err(failure(self.terminal.clone()).await),
        }
    }
}
impl<T> Drop for FileTask<T> {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
pub(super) struct TaskState<T> {
    pub progress: watch::Sender<u64>,
    pub completion: oneshot::Sender<Result<T>>,
    pub cancel: CancellationToken,
}
pub(super) fn task<T>(
    id: String,
    total: u64,
    terminal: watch::Receiver<Option<Arc<FileError>>>,
) -> (FileTask<T>, TaskState<T>) {
    let (progress, updates) = watch::channel(0);
    let (completion, result) = oneshot::channel();
    let cancel = CancellationToken::new();
    (
        FileTask {
            id,
            total,
            progress: updates,
            completion: Some(result),
            cancel: cancel.clone(),
            terminal,
        },
        TaskState {
            progress,
            completion,
            cancel,
        },
    )
}
pub(super) async fn failure(mut terminal: watch::Receiver<Option<Arc<FileError>>>) -> FileError {
    loop {
        if let Some(error) = terminal.borrow().clone() {
            return FileError::Failed(error);
        }
        if terminal.changed().await.is_err() {
            return FileError::Closed;
        }
    }
}
