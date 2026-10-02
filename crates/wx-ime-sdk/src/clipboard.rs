use crate::{Client, Error, Result, SendReceipt, cos::MAX_IMAGE_BYTES, error::business};
use std::path::Path;
use wx_ime_protocol::{
    clipboard::{self, CosUrl, ImageReference},
    crypto::ImageKey,
    transport, wire,
};

impl Client {
    async fn paired_group(&self) -> Result<u64> {
        let mut session = self.session().await?;
        let group = session.group(1).await?.ok_or(Error::NotPaired)?;
        let own = session.store.state.uin;
        if !group.devices.iter().any(|d| d.uin == Some(own))
            || !group
                .devices
                .iter()
                .any(|d| d.uin.is_some_and(|uin| uin != own))
        {
            return Err(Error::NotPaired);
        }
        group
            .group_id
            .filter(|id| *id > 0)
            .ok_or(Error::Invalid("missing group ID"))
    }
    async fn upload(&self, group: u64, bytes: Vec<u8>) -> Result<CosUrl> {
        let credentials: wire::CosResponse = {
            let mut session = self.session().await?;
            let request = wire::CosRequest {
                upload_biz: Some(6),
                ext: Some(
                    serde_json::json!({"log_fully_custom_path": session.timestamp()?.to_string()})
                        .to_string(),
                ),
                method: Some(3),
            };
            session.api(transport::COS_CREDENTIALS, request).await?
        };
        let limit = credentials
            .size_limit_kib
            .filter(|n| *n > 0)
            .ok_or(Error::Invalid("COS upload limit missing"))?
            .checked_mul(1024)
            .ok_or(Error::Invalid("COS upload limit overflow"))?;
        if bytes.len() as u64 > limit {
            return Err(Error::Invalid("object exceeds server COS limit"));
        }
        let upload = CosUrl::parse(
            credentials
                .upload_url
                .as_deref()
                .ok_or(Error::Invalid("COS upload URL missing"))?,
        )?;
        let download = CosUrl::parse(
            credentials
                .download_url
                .as_deref()
                .ok_or(Error::Invalid("COS download URL missing"))?,
        )?;
        self.inner.cos.put(&upload, bytes).await?;
        // Network work happens outside the control lock. Publication revalidates the group.
        if self.paired_group().await? != group {
            return Err(Error::Invalid("group changed during upload"));
        }
        Ok(download)
    }
    async fn publish(&self, group: u64, kind: u32, bytes: Vec<u8>) -> Result<SendReceipt> {
        let mut session = self.session().await?;
        let current = session.group(1).await?.ok_or(Error::NotPaired)?;
        if current.group_id != Some(group)
            || !current
                .devices
                .iter()
                .any(|d| d.uin == Some(session.store.state.uin))
        {
            return Err(Error::Invalid("group changed before publication"));
        }
        let trace_id = uuid::Uuid::new_v4().simple().to_string();
        let msg = wire::ClipboardMsg {
            r#type: Some(kind),
            binary_data: Some(bytes),
            trace_id: Some(trace_id.clone()),
            ..Default::default()
        };
        let request = wire::SendClipboardRequest {
            uin: Some(session.store.state.uin),
            platform: Some(1),
            msg: Some(msg),
            group_id: Some(group),
        };
        let response: wire::SendClipboardResponse =
            match session.api(transport::SEND_CLIPBOARD, request).await {
                Ok(response) => response,
                Err(_) => return Err(Error::OutcomeUnknown),
            };
        business(transport::SEND_CLIPBOARD, response.status)?;
        let version = response
            .version
            .filter(|v| *v > 0)
            .ok_or(Error::OutcomeUnknown)?;
        Ok(SendReceipt {
            group,
            version,
            trace_id,
        })
    }
    pub async fn send_text(&self, text: &str) -> Result<SendReceipt> {
        if text.is_empty() || text.len() > clipboard::MAX_TEXT_BYTES {
            return Err(Error::Invalid("text must be 1..1048576 UTF-8 bytes"));
        }
        let group = self.paired_group().await?;
        if text.len() <= clipboard::DEFAULT_TEXT_INLINE_BYTES {
            self.publish(group, clipboard::TEXT_INLINE, text.as_bytes().to_vec())
                .await
        } else {
            let url = self.upload(group, text.as_bytes().to_vec()).await?;
            self.publish(group, clipboard::TEXT_COS, url.expose().as_bytes().to_vec())
                .await
        }
    }
    /// Send an already encoded image. No transcoding and no host clipboard writes.
    pub async fn send_image(&self, path: impl AsRef<Path>) -> Result<SendReceipt> {
        let path = path.as_ref();
        let bytes = crate::storage::read_limited(path, MAX_IMAGE_BYTES).await?;
        if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
            return Err(Error::Invalid("image empty or exceeds client limit"));
        }
        let group = self.paired_group().await?;
        let mut key = [0; 16];
        openssl::rand::rand_bytes(&mut key).map_err(wx_ime_protocol::Error::from)?;
        let key = ImageKey::new(key);
        let url = self.upload(group, key.apply(&bytes)?).await?;
        let reference = ImageReference::new(&bytes, url, key)?;
        self.publish(group, clipboard::IMAGE_COS, reference.encode()?)
            .await
    }
}
