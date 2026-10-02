//! One incoming file owns validation, private temporary data and durable commit.
//! The stream worker owns acknowledgment and delivery of the committed result.
use super::{
    BLOCK, FileDownload, FileError, FileInfo, ReceivedFile, Result, id,
    task::{TaskState, task},
};
use std::{path::Path, sync::Arc};
use tempfile::{NamedTempFile, TempDir};
use tokio::{
    fs::File,
    io::AsyncWriteExt,
    sync::{oneshot, watch},
};
use wx_ime_protocol::flurry::FileChunk;

type Committed = (ReceivedFile, oneshot::Sender<Result<ReceivedFile>>);
pub(super) struct Incoming {
    file: File,
    temporary: NamedTempFile,
    directory: TempDir,
    digest: blake3::Hasher,
    pub(super) info: FileInfo,
    offset: u64,
    pub(super) state: TaskState<ReceivedFile>,
}
impl Incoming {
    /// Called only after validated metadata has reserved the bounded host slot.
    pub(super) fn new(
        dir: &Path,
        info: FileInfo,
        terminal: watch::Receiver<Option<Arc<FileError>>>,
    ) -> Result<(Self, FileDownload)> {
        let directory = tempfile::Builder::new()
            .prefix("wx-ime-file-")
            .tempdir_in(dir)?;
        let temporary = NamedTempFile::new_in(directory.path())?;
        let file = File::from_std(temporary.as_file().try_clone()?);
        let (handle, state) = task(id(&info)?.to_owned(), info.size.unwrap_or(0), terminal);
        Ok((
            Self {
                file,
                temporary,
                directory,
                digest: blake3::Hasher::new(),
                info,
                offset: 0,
                state,
            },
            handle,
        ))
    }
    /// Release temporary resources before reporting cancellation to the host.
    pub(super) fn cancel(self, id: String) {
        drop(self.file);
        drop(self.temporary);
        drop(self.directory);
        let _ = self.state.completion.send(Err(FileError::Canceled { id }));
    }
    /// Return the validated range only after its bytes have been written and synced.
    pub(super) async fn append(&mut self, chunk: &FileChunk) -> Result<(u64, u64)> {
        let info = chunk
            .action
            .as_ref()
            .and_then(|action| action.file.as_ref())
            .ok_or(FileError::Invalid("missing data file"))?;
        if info != &self.info {
            return Err(FileError::Invalid("file metadata changed"));
        }
        let data = chunk
            .data
            .as_deref()
            .ok_or(FileError::Invalid("missing file data"))?;
        let start = chunk.start.ok_or(FileError::Invalid("missing start"))?;
        let end = chunk.end.ok_or(FileError::Invalid("missing end"))?;
        if data.len() > BLOCK
            || start != self.offset
            || end
                != start
                    .checked_add(data.len() as u64)
                    .ok_or(FileError::Invalid("range overflow"))?
            || end > self.info.size.unwrap_or(0)
        {
            return Err(FileError::Invalid("file range"));
        }
        self.digest.update(data);
        self.file.write_all(data).await?;
        // Surface buffered write errors before syncing, progress or wire ACK.
        self.file.flush().await?;
        self.file.sync_data().await?;
        self.offset = end;
        self.state.progress.send_replace(end);
        Ok((start, end))
    }
    /// Keep the completed directory only after digest, file and directory durability.
    /// Caller sends the final range ACK before completing the returned host channel.
    pub(super) async fn commit(self, dir: &Path) -> Result<Committed> {
        let file_id = id(&self.info)?.to_owned();
        if self.digest.finalize().to_hex().as_str() != file_id {
            return Err(FileError::Invalid("BLAKE3 content digest mismatch"));
        }
        self.file.sync_all().await?;
        drop(self.file);
        let destination = self
            .directory
            .path()
            .join(self.info.name.as_deref().unwrap_or("file"));
        self.temporary
            .persist_noclobber(&destination)
            .map_err(|error| error.error)?;
        #[cfg(unix)]
        {
            std::fs::File::open(self.directory.path())?.sync_all()?;
            std::fs::File::open(dir)?.sync_all()?;
        }
        let _ = self.directory.keep();
        Ok((
            ReceivedFile {
                id: file_id,
                path: destination,
                bytes: self.offset,
            },
            self.state.completion,
        ))
    }
}
