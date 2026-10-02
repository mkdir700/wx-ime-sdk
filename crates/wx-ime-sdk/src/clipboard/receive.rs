use crate::{
    Client, ClipboardContent, ClipboardInfo, DownloadedImage, Error, Event, Group,
    ReceivedClipboard, ReceivedId, Result, clipboard::cos::MAX_IMAGE_BYTES, storage::private_dir,
};
use std::{
    fs::{self, File},
    io::Write,
    time::Duration,
};
use wx_ime_protocol::{
    clipboard::{self, CosUrl, ImageReference},
    crypto::sha256,
};

#[derive(Default)]
pub(crate) struct ReceiveState {
    group: Option<Group>,
    emitted: Option<ReceivedId>,
}
impl Client {
    /// Consumer-driven latest-value polling. One unacknowledged receipt at a time.
    /// Cancellation is safe; calling again continues from durable pending state.
    pub async fn next_event(&self) -> Result<Event> {
        let mut receive = self.inner.receive.lock().await;
        loop {
            if let Some(event) = self.poll_step(&mut receive).await? {
                return Ok(event);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    /// One non-blocking poll of the latest clipboard value. Returns the durable pending
    /// receipt (possibly from an earlier process) or a newer remote value, `None` when
    /// there is nothing new. The receipt must still be acknowledged after it is committed.
    pub async fn poll_clipboard_once(&self) -> Result<Option<ReceivedClipboard>> {
        let mut receive = self.inner.receive.lock().await;
        loop {
            match self.poll_step(&mut receive).await? {
                Some(Event::ClipboardReceived(received)) => return Ok(Some(received)),
                Some(Event::GroupChanged(_)) => {}
                None => return Ok(None),
            }
        }
    }
    /// Read-only view of the server's latest clipboard value and why it would or would not be
    /// delivered. For diagnostics; it never changes receive state.
    pub async fn inspect_clipboard(&self) -> Result<ClipboardInfo> {
        let mut session = self.session().await?;
        let own = session.store.state.uin;
        let now_ms = session.timestamp()?;
        let wire_group = session.group(3).await?.ok_or(Error::NotPaired)?;
        let group_id = wire_group.group_id.unwrap_or(0);
        let consumed = session
            .store
            .state
            .consumed
            .get(&group_id)
            .copied()
            .unwrap_or(0);
        let message = wire_group.clipboard.as_ref();
        let expire_at = message.and_then(|m| m.expire_at).unwrap_or(0);
        let from_uin = message.and_then(|m| m.from_uin).unwrap_or(0);
        let version = message.and_then(|m| m.version).unwrap_or(0);
        Ok(ClipboardInfo {
            has_message: message.is_some(),
            version,
            from_uin,
            kind: message.and_then(|m| m.r#type).unwrap_or(0),
            expire_at,
            now_ms,
            consumed,
            from_self: from_uin == own,
            expired: expire_at.saturating_mul(1000) <= now_ms as u64,
            already_consumed: version <= consumed,
        })
    }
    async fn poll_step(&self, receive: &mut ReceiveState) -> Result<Option<Event>> {
        {
            let mut session = self.session().await?;
            if let Some(pending) = session.store.state.pending.clone() {
                if receive.emitted.as_ref() != Some(&pending.id) {
                    if let ClipboardContent::Image(image) = &pending.content {
                        let bytes =
                            crate::storage::read_limited(&image.path, MAX_IMAGE_BYTES).await?;
                        if bytes.len() != image.bytes || sha256(&bytes) != image.sha256 {
                            return Err(Error::Invalid("pending image cache integrity"));
                        }
                    }
                    receive.emitted = Some(pending.id.clone());
                    return Ok(Some(Event::ClipboardReceived(pending)));
                }
                return Ok(None);
            }
            let Some(wire_group) = session.group(3).await? else {
                return Ok(None);
            };
            let group = Group::try_from(&wire_group)?;
            if receive.group.as_ref() != Some(&group) {
                receive.group = Some(group.clone());
                return Ok(Some(Event::GroupChanged(group)));
            }
            let own = session.store.state.uin;
            if !group.devices.iter().any(|d| d.uin == own) {
                return Err(Error::Invalid("clipboard group missing local member"));
            }
            let cursor = session
                .store
                .state
                .consumed
                .get(&group.id)
                .copied()
                .unwrap_or(0);
            let time_ms = session.timestamp()?;
            let cache_dir = session.store.directory.join("cache");
            drop(session);
            if let Some(message) = wire_group.clipboard {
                let version = message.version.unwrap_or(0);
                let from_uin = message.from_uin.unwrap_or(0);
                let expiry = message.expire_at.unwrap_or(0);
                if version > cursor
                    && from_uin != 0
                    && from_uin != own
                    && expiry.saturating_mul(1000) > time_ms as u64
                {
                    let source = group
                        .devices
                        .iter()
                        .find(|d| d.uin == from_uin)
                        .cloned()
                        .ok_or(Error::Invalid("clipboard source is not a group member"))?;
                    let id = ReceivedId {
                        group: group.id,
                        version,
                        from_uin,
                        trace_id: message
                            .trace_id
                            .filter(|s| !s.is_empty())
                            .ok_or(Error::Invalid("clipboard trace ID missing"))?,
                    };
                    let bytes = message
                        .binary_data
                        .ok_or(Error::Invalid("clipboard binary payload missing"))?;
                    let content = match message.r#type {
                        Some(clipboard::TEXT_INLINE) => {
                            if bytes.len() > clipboard::MAX_TEXT_BYTES {
                                return Err(Error::Invalid("received text exceeds limit"));
                            }
                            ClipboardContent::Text(
                                String::from_utf8(bytes)
                                    .map_err(|_| Error::Invalid("text UTF-8"))?,
                            )
                        }
                        Some(clipboard::TEXT_COS) => {
                            let url = CosUrl::parse(
                                std::str::from_utf8(&bytes)
                                    .map_err(|_| Error::Invalid("text URL UTF-8"))?,
                            )?;
                            let bytes = self.inner.cos.get(&url, clipboard::MAX_TEXT_BYTES).await?;
                            ClipboardContent::Text(
                                String::from_utf8(bytes)
                                    .map_err(|_| Error::Invalid("text UTF-8"))?,
                            )
                        }
                        Some(clipboard::IMAGE_COS) => {
                            let reference = ImageReference::decode(&bytes)?;
                            let cipher =
                                self.inner.cos.get(&reference.url, MAX_IMAGE_BYTES).await?;
                            let plain = reference.decrypt(&cipher)?;
                            let digest = sha256(&plain);
                            private_dir(&cache_dir)?;
                            let path = cache_dir
                                .join(format!("{}-{}-{}.image", group.id, version, digest));
                            let mut temp = tempfile::NamedTempFile::new_in(&cache_dir)?;
                            temp.write_all(&plain)?;
                            temp.as_file().sync_all()?;
                            if let Err(error) = temp.persist_noclobber(&path)
                                && (error.error.kind() != std::io::ErrorKind::AlreadyExists
                                    || sha256(&fs::read(&path)?) != digest)
                            {
                                return Err(Error::Io(error.error));
                            }
                            #[cfg(unix)]
                            File::open(&cache_dir)?.sync_all()?;
                            ClipboardContent::Image(DownloadedImage {
                                path,
                                bytes: plain.len(),
                                sha256: digest,
                            })
                        }
                        _ => return Err(Error::Invalid("unsupported clipboard message type")),
                    };
                    let pending = ReceivedClipboard {
                        id,
                        source,
                        content,
                        expire_at: expiry,
                    };
                    let mut session = self.session().await?;
                    session.store.state.pending = Some(pending.clone());
                    if let Err(error) = session.store.save() {
                        session.store.state.pending = None;
                        return Err(error);
                    }
                    receive.emitted = Some(pending.id.clone());
                    return Ok(Some(Event::ClipboardReceived(pending)));
                }
            }
            Ok(None)
        }
    }
    /// Commit the per-group cursor only after the host has committed the receipt.
    pub async fn acknowledge(&self, id: ReceivedId) -> Result<()> {
        let mut session = self.session().await?;
        let pending = session
            .store
            .state
            .pending
            .take()
            .ok_or(Error::Invalid("no pending receipt"))?;
        if pending.id != id {
            session.store.state.pending = Some(pending);
            return Err(Error::Invalid(
                "acknowledgment does not match pending receipt",
            ));
        }
        let previous = session.store.state.consumed.insert(id.group, id.version);
        if let Err(error) = session.store.save() {
            if let Some(value) = previous {
                session.store.state.consumed.insert(id.group, value);
            } else {
                session.store.state.consumed.remove(&id.group);
            }
            session.store.state.pending = Some(pending);
            return Err(error);
        }
        // Acknowledgment is already durable. Cleanup failure must not turn a
        // committed receipt into an apparent failure; open() retries collection.
        let _ = session.store.clean_cache();
        Ok(())
    }
}
