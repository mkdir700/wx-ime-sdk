//! Bounded immutable upload data, prepared independently of the connection worker.
use super::{BLOCK, FileError, FileInfo, MAX_FILE, Result, name};
use std::path::Path;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
};

pub(super) async fn snapshot(path: &Path) -> Result<(File, FileInfo)> {
    let mut source = File::open(path).await?;
    let meta = source.metadata().await?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return Err(FileError::Invalid("file size or type"));
    }
    let filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(FileError::Invalid("UTF-8 file name"))?;
    name(filename)?;
    // Existing tempfile provides a private anonymous file, removed on handle close.
    let mut copy = File::from_std(tempfile::tempfile()?);
    let mut digest = blake3::Hasher::new();
    let mut buffer = vec![0; BLOCK];
    let mut bytes = 0u64;
    loop {
        let n = source.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > MAX_FILE {
            return Err(FileError::Invalid("file grew beyond limit"));
        }
        copy.write_all(&buffer[..n]).await?;
        digest.update(&buffer[..n]);
    }
    if bytes != meta.len() {
        return Err(FileError::Invalid("file changed while preparing"));
    }
    copy.flush().await?;
    copy.rewind().await?;
    Ok((
        copy,
        FileInfo {
            id: Some(digest.finalize().to_hex().to_string()),
            name: Some(filename.to_owned()),
            size: Some(bytes),
        },
    ))
}
