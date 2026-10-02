use crate::{
    Error, Group, LocalDevice, PairingCode, Result, error::business, session::Session,
    storage::Store,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, MutexGuard};
use wx_ime_protocol::{crypto::sha256, transport, wire};

/// One persistent identity directory, one active client. Capture is opt-in and sensitive.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    pub state_dir: PathBuf,
    pub device_name: String,
    pub capture_dir: Option<PathBuf>,
}
impl ClientOptions {
    pub fn new(state_dir: impl Into<PathBuf>, device_name: impl Into<String>) -> Self {
        Self {
            state_dir: state_dir.into(),
            device_name: device_name.into(),
            capture_dir: None,
        }
    }
}
pub(crate) struct Inner {
    pub session: Mutex<Session>,
    pub closed: AtomicBool,
    pub lifetime: tokio_util::sync::CancellationToken,
    pub pairing: AtomicBool,
    pub cos: crate::cos::Cos,
    pub receive: Mutex<crate::receive::ReceiveState>,
}
#[derive(Clone)]
pub struct Client {
    pub(crate) inner: Arc<Inner>,
}
impl Client {
    pub async fn open(options: ClientOptions) -> Result<Self> {
        let store = Store::open(options.state_dir, options.device_name)?;
        let cos = crate::cos::Cos::new(options.capture_dir.clone())?;
        let session = Session::open(store, options.capture_dir).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                session: Mutex::new(session),
                closed: AtomicBool::new(false),
                lifetime: tokio_util::sync::CancellationToken::new(),
                pairing: AtomicBool::new(false),
                cos,
                receive: Mutex::new(crate::receive::ReceiveState::default()),
            }),
        })
    }
    pub(crate) async fn session(&self) -> Result<MutexGuard<'_, Session>> {
        let guard = self.inner.session.lock().await;
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(Error::Closed);
        }
        Ok(guard)
    }
    pub async fn local_device(&self) -> Result<LocalDevice> {
        let session = self.session().await?;
        Ok(LocalDevice {
            uin: session.store.state.uin,
            name: session.store.state.device_name.clone(),
            public_key_sha256: sha256(session.identity.public_hex()?.as_bytes()),
        })
    }
    pub async fn group(&self) -> Result<Option<Group>> {
        self.session()
            .await?
            .group(1)
            .await?
            .as_ref()
            .map(Group::try_from)
            .transpose()
    }
    pub async fn devices(&self) -> Result<Vec<crate::Device>> {
        Ok(self
            .group()
            .await?
            .map(|group| group.devices)
            .unwrap_or_default())
    }
    pub async fn reconnect(&self) -> Result<()> {
        self.session().await?.initialize().await
    }
    pub(crate) async fn issue_code(&self) -> Result<PairingCode> {
        let mut session = self.session().await?;
        let request = wire::CodeRequest {
            uin: Some(session.store.state.uin),
            platform: Some(1),
            device: Some(wire::Device {
                name: Some(session.store.state.device_name.clone()),
                platform: Some(1),
            }),
        };
        let response: wire::CodeResponse = session.api(transport::REQUEST_CODE, request).await?;
        business(transport::REQUEST_CODE, response.status)?;
        Ok(PairingCode::parse(
            response
                .code
                .ok_or(Error::Invalid("pairing code missing"))?,
        )?)
    }
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.lifetime.cancel();
        self.inner.session.lock().await.store.release();
        Ok(())
    }
}
