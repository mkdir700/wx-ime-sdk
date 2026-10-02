use crate::{
    Error, Result,
    crypto::{ImageKey, md5},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};

pub const TEXT_INLINE: u32 = 1;
pub const TEXT_COS: u32 = 2;
pub const IMAGE_COS: u32 = 6;
pub const DEFAULT_TEXT_INLINE_BYTES: usize = 256 * 1024;
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;

/// Validated COS URL. Debug never reveals signed query parameters.
#[derive(Clone)]
pub struct CosUrl(url::Url);
impl CosUrl {
    pub fn parse(value: &str) -> Result<Self> {
        let url = url::Url::parse(value).map_err(|_| Error::Invalid("COS URL"))?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
            || !url
                .host_str()
                .is_some_and(|host| host.ends_with(".myqcloud.com"))
            || url.fragment().is_some()
        {
            return Err(Error::Invalid("COS URL origin"));
        }
        Ok(Self(url))
    }
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }
}
impl std::fmt::Debug for CosUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CosUrl([REDACTED])")
    }
}

#[derive(Serialize, Deserialize)]
struct ImageJson {
    version: u32,
    path: String,
    md5: String,
    key: String,
}
/// Verified type6 format. Legacy raw URLs are deliberately not silently accepted.
#[derive(Debug, Clone)]
pub struct ImageReference {
    pub url: CosUrl,
    pub md5: String,
    key: ImageKey,
}
impl ImageReference {
    pub fn new(plain: &[u8], url: CosUrl, key: ImageKey) -> Result<Self> {
        Ok(Self {
            url,
            md5: md5(plain)?,
            key,
        })
    }
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&ImageJson {
            version: 1,
            path: self.url.expose().into(),
            md5: self.md5.clone(),
            key: STANDARD.encode(self.key.bytes()),
        })?)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let obj: ImageJson = serde_json::from_slice(bytes)?;
        if obj.version != 1
            || obj.md5.len() != 32
            || !obj.md5.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::Invalid("image reference version/MD5"));
        }
        let key: [u8; 16] = STANDARD
            .decode(obj.key)
            .map_err(|_| Error::Invalid("image key base64"))?
            .try_into()
            .map_err(|_| Error::Invalid("image key length"))?;
        Ok(Self {
            url: CosUrl::parse(&obj.path)?,
            md5: obj.md5.to_lowercase(),
            key: ImageKey::new(key),
        })
    }
    pub fn decrypt(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let plain = self.key.apply(bytes)?;
        if md5(&plain)? != self.md5 {
            return Err(Error::Invalid("image plaintext MD5 mismatch"));
        }
        Ok(plain)
    }
}
