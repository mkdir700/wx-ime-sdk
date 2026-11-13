use crate::{Error, Result, capture::Capture, error::business, storage::Store};
use prost::Message;
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wx_ime_protocol::{
    crypto::{ControlKey, IdentityKey},
    packet::{self, RequestContext},
    transport::{self, Endpoint},
    wire,
};

pub(crate) fn now() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Invalid("system clock"))?
        .as_millis()
        .try_into()
        .map_err(|_| Error::Invalid("system clock range"))
}
pub(crate) fn random_hex(size: usize) -> Result<String> {
    let mut bytes = vec![0; size];
    openssl::rand::rand_bytes(&mut bytes).map_err(wx_ime_protocol::Error::from)?;
    Ok(hex::encode(bytes))
}
pub(crate) struct Session {
    pub store: Store,
    pub identity: IdentityKey,
    http: reqwest::Client,
    key: Option<ControlKey>,
    pub offset: i64,
    task: u32,
    capture: Option<Capture>,
}
impl Session {
    pub async fn open(store: Store, capture: Option<PathBuf>) -> Result<Self> {
        let identity = IdentityKey::from_pem(store.state.private_pem.as_bytes())?;
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| Error::Transport)?;
        let mut session = Self {
            store,
            identity,
            http,
            key: None,
            offset: 0,
            task: 1,
            capture: capture.map(Capture::new).transpose()?,
        };
        session.initialize().await?;
        Ok(session)
    }
    pub fn device_info(&self) -> String {
        format!(
            "UniClipboard;research;standalone;{};sdk 34;PHONE",
            self.store.state.device_name
        )
    }
    pub fn timestamp(&self) -> Result<i64> {
        now()?
            .checked_add(self.offset)
            .ok_or(Error::Invalid("timestamp overflow"))
    }
    async fn request(
        &mut self,
        endpoint: Endpoint,
        plain: &[u8],
        key: Option<&ControlKey>,
        task: u32,
        token: &str,
    ) -> Result<Vec<u8>> {
        let trace = random_hex(8)?;
        let packet = packet::encode(
            endpoint,
            plain,
            key,
            RequestContext {
                timestamp_ms: self.timestamp()?,
                trace_id: &trace,
                task,
                uin: self.store.state.uin,
                registration_token: token,
            },
        )?;
        let capture_index = self
            .capture
            .as_mut()
            .map(|capture| capture.request(&packet))
            .transpose()?;
        let mut request = self
            .http
            .post(format!("{}{}", transport::HOST, endpoint.path()));
        for (name, value) in packet.headers {
            request = request.header(name, value);
        }
        let mut response = request
            .body(packet.body)
            .send()
            .await
            .map_err(|_| Error::Transport)?;
        let status = response.status();
        let compression = response
            .headers()
            .get("Kb-CompressionType")
            .map(|v| v.to_str())
            .transpose()
            .map_err(|_| Error::Invalid("compression header"))?
            .unwrap_or("1")
            .to_string();
        let mut raw = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
            if raw.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Error::Invalid("control response size limit"));
            }
            raw.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            return Err(Error::Http(status.as_u16()));
        }
        let plain = packet::decode(&raw, key, &compression, 2 * 1024 * 1024)?;
        if let (Some(capture), Some(index)) = (&self.capture, capture_index) {
            capture.response(index, endpoint, status.as_u16(), &compression, &raw, &plain)?;
        }
        Ok(plain)
    }
    pub async fn initialize(&mut self) -> Result<()> {
        self.key = None;
        let started = now()?;
        let response = self.request(Endpoint::Timestamp, &[], None, 0, "").await?;
        let stamp = wire::Timestamp::decode(response.as_slice())
            .map_err(wx_ime_protocol::Error::from)?
            .milliseconds
            .filter(|ms| *ms > 0)
            .ok_or(Error::Invalid("server timestamp missing"))?;
        self.offset = i64::try_from(stamp).map_err(|_| Error::Invalid("server timestamp range"))?
            - (started + now()?) / 2;
        let state = &self.store.state;
        let request = wire::EcdhRequest {
            public_key: Some(self.identity.public_hex()?),
            previous_server_key: Some(state.server_key.clone()),
            need_uin: Some(state.uin == 0),
            device_code: Some(state.device_code.clone()),
            uin: (state.uin != 0).then_some(state.uin),
            uin_token: (state.uin == 0).then(String::new),
            device_token: Some(String::new()),
            device_info: Some(self.device_info()),
            device_name: Some(state.device_name.clone()),
        };
        let response = self
            .request(
                Endpoint::Ecdh,
                &request.encode_to_vec(),
                Some(&ControlKey::bootstrap()),
                2147483646,
                "",
            )
            .await?;
        let response = wire::EcdhResponse::decode(response.as_slice())
            .map_err(wx_ime_protocol::Error::from)?;
        business(0, response.status)?;
        let server_key = response
            .public_key
            .filter(|s| !s.is_empty())
            .ok_or(Error::Invalid("server public key missing"))?;
        let key = self.identity.agree(&server_key)?;
        if self.store.state.uin == 0 {
            let token = response
                .gen_uin_token
                .filter(|s| !s.is_empty())
                .ok_or(Error::Invalid("registration token missing"))?;
            let request = wire::UinRequest {
                device_code: Some(self.store.state.device_code.clone()),
                server_key: Some(server_key.clone()),
                uin: None,
                uuid: Some(self.store.state.uuid.clone()),
                device_token: Some(String::new()),
            };
            let response = self
                .request(
                    Endpoint::Register,
                    &request.encode_to_vec(),
                    Some(&key),
                    2147483645,
                    &token,
                )
                .await?;
            let response = wire::UinResponse::decode(response.as_slice())
                .map_err(wx_ime_protocol::Error::from)?;
            business(0, response.status)?;
            self.store.state.uin = response
                .uin
                .filter(|uin| *uin > 0)
                .ok_or(Error::Invalid("registered Uin missing"))?;
        }
        self.store.state.server_key = server_key;
        self.store.save()?;
        self.key = Some(key);
        Ok(())
    }
    pub async fn api<M: Message, R: Message + Default>(
        &mut self,
        command: u32,
        message: M,
    ) -> Result<R> {
        let key = self
            .key
            .clone()
            .ok_or(Error::Invalid("session not initialized"))?;
        let plain = packet::envelope(
            self.store.state.uin,
            command,
            &self.device_info(),
            self.timestamp()?,
            message.encode_to_vec(),
        );
        let task = self.task;
        self.task = self
            .task
            .checked_add(1)
            .ok_or(Error::Invalid("session task counter exhausted"))?;
        let response = self
            .request(Endpoint::Api(command), &plain, Some(&key), task, "")
            .await?;
        Ok(R::decode(packet::business_body(&response)?.as_slice())
            .map_err(wx_ime_protocol::Error::from)?)
    }
    pub async fn group(&mut self, filter: u32) -> Result<Option<wire::Group>> {
        let request = wire::GroupRequest {
            uin: Some(self.store.state.uin),
            platform: Some(1),
            filter: Some(filter),
            clipboard_version: (filter & 2 != 0).then_some(0),
            current_transfer_code: None,
        };
        let response: wire::GroupResponse = self.api(transport::GROUP, request).await?;
        if response.status == Some(10020) && response.group.is_none() {
            return Ok(None);
        }
        business(transport::GROUP, response.status)?;
        Ok(Some(
            response
                .group
                .ok_or(Error::Invalid("group response missing group"))?,
        ))
    }
}
