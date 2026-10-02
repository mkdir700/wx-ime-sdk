use crate::{
    Error, Result,
    storage::{private_dir, write_new},
};
use serde_json::json;
use std::{path::PathBuf, time::Duration};
use wx_ime_protocol::{clipboard::CosUrl, crypto::sha256};

pub(crate) const MAX_IMAGE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) struct Cos {
    http: reqwest::Client,
    capture: Option<PathBuf>,
}
impl Cos {
    pub fn new(capture: Option<PathBuf>) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .map_err(|_| Error::CosTransport)?,
            capture,
        })
    }
    fn capture(&self, method: &str, url: &CosUrl, status: u16, body: &[u8]) -> Result<()> {
        if let Some(directory) = &self.capture {
            let directory = directory.join("objects");
            private_dir(&directory)?;
            let id = uuid::Uuid::new_v4().simple().to_string();
            write_new(&directory.join(format!("{id}.bin")), body)?;
            write_new(
                &directory.join(format!("{id}.json")),
                &serde_json::to_vec_pretty(
                    &json!({"method": method, "http_status": status, "url_sha256": sha256(url.expose().as_bytes()), "body_sha256": sha256(body), "bytes": body.len()}),
                )?,
            )?;
        }
        Ok(())
    }
    pub async fn put(&self, url: &CosUrl, bytes: Vec<u8>) -> Result<()> {
        let response = self
            .http
            .put(url.expose())
            .header("Content-Type", "application/octet-stream")
            .body(bytes.clone())
            .send()
            .await
            .map_err(|_| Error::CosTransport)?;
        let status = response.status().as_u16();
        if !matches!(status, 200 | 201 | 204) {
            return Err(Error::CosHttp(status));
        }
        self.capture("PUT", url, status, &bytes)
    }
    pub async fn get(&self, url: &CosUrl, limit: usize) -> Result<Vec<u8>> {
        let mut response = self
            .http
            .get(url.expose())
            .send()
            .await
            .map_err(|_| Error::CosTransport)?;
        if response.status().as_u16() != 200 {
            return Err(Error::CosHttp(response.status().as_u16()));
        }
        if response
            .content_length()
            .is_some_and(|len| len > limit as u64)
        {
            return Err(Error::Invalid("COS download exceeds limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::CosTransport)? {
            if bytes.len() + chunk.len() > limit {
                return Err(Error::Invalid("COS download exceeds limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(Error::Invalid("empty COS object"));
        }
        self.capture("GET", url, 200, &bytes)?;
        Ok(bytes)
    }
}
