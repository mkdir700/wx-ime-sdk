use crate::{Error, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use wx_ime_protocol::{
    crypto::{IdentityKey, md5},
    transport::SIGN_SALT,
};
use zeroize::{Zeroize, Zeroizing};

// JSON control-character escapes can use six bytes per UTF-8 input byte.
// Keep space for identity/receipt metadata and enforce the same read/write limit.
const MAX_STATE_BYTES: usize = wx_ime_protocol::clipboard::MAX_TEXT_BYTES * 6 + 2 * 1024 * 1024;

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
pub(crate) fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
pub(crate) async fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await?;
    let meta = file.metadata().await?;
    if !meta.is_file() || meta.len() > limit as u64 {
        return Err(Error::Invalid("file type or size limit"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes).await?;
    if bytes.len() > limit {
        return Err(Error::Invalid("file size limit"));
    }
    Ok(bytes)
}
#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    pub format: u32,
    pub device_name: String,
    pub private_pem: String,
    pub device_code: String,
    pub uuid: String,
    pub uin: u64,
    pub server_key: String,
    #[serde(default)]
    pub consumed: BTreeMap<u64, u64>,
    #[serde(default)]
    pub pending: Option<crate::ReceivedClipboard>,
}
impl Drop for State {
    fn drop(&mut self) {
        self.private_pem.zeroize();
    }
}
pub(crate) struct Store {
    pub state: State,
    pub directory: PathBuf,
    lock: Option<File>,
}
impl Store {
    pub fn open(directory: PathBuf, device_name: String) -> Result<Self> {
        if device_name.trim().is_empty() || device_name.len() > 128 {
            return Err(Error::Invalid("device name must be 1..128 UTF-8 bytes"));
        }
        private_dir(&directory)?;
        let directory = fs::canonicalize(directory)?;
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(directory.join("identity.lock"))?;
        lock.try_lock_exclusive().map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Error::IdentityInUse
            } else {
                Error::Io(e)
            }
        })?;
        let path = directory.join("identity.json");
        let state = if path.exists() {
            let mut bytes = Zeroizing::new(Vec::new());
            File::open(&path)?
                .take(MAX_STATE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > MAX_STATE_BYTES {
                return Err(Error::Invalid("identity state exceeds limit"));
            }
            let state: State = serde_json::from_slice(&bytes)?;
            if state.format != 1 || state.device_name != device_name {
                return Err(Error::Invalid("identity format or name differs"));
            }
            let code = &state.device_code;
            if !code.is_ascii()
                || code.len() != 64
                || !code.starts_with("AND")
                || code[32..]
                    != md5(format!("{}{SIGN_SALT}", &code[..32]).as_bytes())?.to_uppercase()
            {
                return Err(Error::Invalid("device code checksum"));
            }
            IdentityKey::from_pem(state.private_pem.as_bytes())?;
            uuid::Uuid::parse_str(&state.uuid).map_err(|_| Error::Invalid("identity UUID"))?;
            state
        } else {
            let key = IdentityKey::generate()?;
            let suffix = uuid::Uuid::new_v4().simple().to_string()[..12].to_owned();
            let prefix = format!("AND{}{suffix}", &"UniClipboard;research"[..17]);
            State {
                format: 1,
                device_name,
                private_pem: String::from_utf8(key.to_pem()?.to_vec())
                    .map_err(|_| Error::Invalid("private key PEM encoding"))?,
                device_code: format!(
                    "{prefix}{}",
                    md5(format!("{prefix}{SIGN_SALT}").as_bytes())?.to_uppercase()
                ),
                uuid: uuid::Uuid::new_v4().to_string(),
                uin: 0,
                server_key: String::new(),
                consumed: BTreeMap::new(),
                pending: None,
            }
        };
        let store = Self {
            state,
            directory,
            lock: Some(lock),
        };
        store.validate_pending()?;
        store.clean_cache()?;
        store.save()?;
        Ok(store)
    }
    pub fn save(&self) -> Result<()> {
        let bytes = Zeroizing::new(serde_json::to_vec_pretty(&self.state)?);
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Error::Invalid("identity state exceeds limit"));
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(self.directory.join("identity.json"))
            .map_err(|e| Error::Io(e.error))?;
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
    pub fn release(&mut self) {
        self.lock.take();
    }
    fn validate_pending(&self) -> Result<()> {
        if let Some(pending) = &self.state.pending {
            if pending.id.group == 0
                || pending.id.version == 0
                || pending.id.from_uin == 0
                || pending.source.uin != pending.id.from_uin
                || self
                    .state
                    .consumed
                    .get(&pending.id.group)
                    .is_some_and(|v| *v >= pending.id.version)
            {
                return Err(Error::Invalid("pending receipt metadata"));
            }
            if let crate::ClipboardContent::Image(image) = &pending.content {
                let expected = self.directory.join("cache").join(format!(
                    "{}-{}-{}.image",
                    pending.id.group, pending.id.version, image.sha256
                ));
                if image.sha256.len() != 64
                    || !image.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                    || image.path != expected
                    || image.bytes == 0
                    || image.bytes > crate::clipboard::cos::MAX_IMAGE_BYTES
                {
                    return Err(Error::Invalid("pending image cache path/size"));
                }
            }
        }
        Ok(())
    }
    /// Reclaim only SDK-named cache objects; never touch a pending receipt.
    pub fn clean_cache(&self) -> Result<()> {
        let cache = self.directory.join("cache");
        if !cache.exists() {
            return Ok(());
        }
        let retained = self.state.pending.as_ref().and_then(|p| {
            if let crate::ClipboardContent::Image(i) = &p.content {
                Some(&i.path)
            } else {
                None
            }
        });
        for entry in fs::read_dir(cache)? {
            let entry = entry?;
            if retained == Some(&entry.path()) || !entry.file_type()?.is_file() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(stem) = name.strip_suffix(".image") else {
                continue;
            };
            let parts: Vec<_> = stem.split('-').collect();
            if parts.len() == 3
                && parts[0].parse::<u64>().is_ok()
                && parts[1].parse::<u64>().is_ok()
                && parts[2].len() == 64
                && parts[2].bytes().all(|b| b.is_ascii_hexdigit())
            {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
}
