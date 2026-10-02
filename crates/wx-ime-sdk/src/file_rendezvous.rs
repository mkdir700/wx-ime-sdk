//! Authenticated P2P file code and peer certificate/address exchange.
//! Application control handshake and transfer cancellation are separate from rendezvous.
use crate::{Client, Error, Result, error::business, file_identity::FileIdentity};
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};
use tokio::time::Instant;
use wx_ime_protocol::wire;
const CAPABILITIES: u64 = 2; // Verified native range acknowledgment; do not advertise unimplemented V2 controls.
#[derive(Clone)]
pub struct FileCode(String);
impl std::fmt::Debug for FileCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileCode([redacted])")
    }
}
impl FileCode {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid("file transfer code"));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    #[serde(default)]
    pub lan: Vec<IpAddr>,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub preferred_client: Option<bool>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LanAddress {
    pub ip: IpAddr,
    pub port: u16,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct AvailableNets {
    #[serde(default)]
    pub lan: Vec<LanAddress>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct GrpcInfo {
    #[serde(default)]
    pub platform: u32,
    pub server_name: String,
    #[serde(default)]
    pub device_name: String,
    pub available_nets: AvailableNets,
}
/// Sensitive rendezvous material intentionally has no Debug implementation.
pub struct FilePeer {
    pub uin: u64,
    pub ca: Vec<u8>,
    pub link: LinkInfo,
    pub grpc: Option<GrpcInfo>,
    pub capabilities: u64,
    pub state: wire::FilePeerState,
    pub ready: bool,
}
impl FilePeer {
    fn decode(peer: wire::FilePeer, expected: u64) -> Result<Self> {
        if peer.uin != Some(expected) || expected == 0 {
            return Err(Error::Invalid("file rendezvous peer identity mismatch"));
        }
        let state = wire::FilePeerState::try_from(peer.state.unwrap_or(0))
            .map_err(|_| Error::Invalid("unknown file peer state"))?;
        match state {
            wire::FilePeerState::Canceled | wire::FilePeerState::FileReadyThenCancel => {
                return Err(Error::Canceled);
            }
            wire::FilePeerState::Rejected => return Err(Error::FileRejected),
            wire::FilePeerState::None | wire::FilePeerState::FileReady => {}
        }
        let ca = peer
            .ca
            .filter(|ca| !ca.is_empty() && ca.len() <= 16 * 1024)
            .ok_or(Error::Invalid("peer CA missing or too large"))?;
        openssl::x509::X509::from_pem(&ca).map_err(wx_ime_protocol::Error::from)?;
        let link = peer
            .link
            .filter(|value| value.len() <= 64 * 1024)
            .ok_or(Error::Invalid("peer link info missing or too large"))?;
        let grpc = match peer.grpc.filter(|value| !value.is_empty()) {
            Some(value) => {
                if value.len() > 64 * 1024 {
                    return Err(Error::Invalid("peer gRPC info too large"));
                }
                Some(serde_json::from_slice(&value)?)
            }
            None => None,
        };
        Ok(Self {
            uin: expected,
            ca,
            link: serde_json::from_slice(&link)?,
            grpc,
            capabilities: peer.capabilities.unwrap_or(0),
            state,
            ready: peer.ready.unwrap_or(false),
        })
    }
}
pub struct FileOffer {
    client: Client,
    peer_uin: u64,
    identity: FileIdentity,
    code: FileCode,
    deadline: Instant,
}
async fn paired(client: &Client, peer_uin: u64) -> Result<()> {
    let local = client.local_device().await?.uin;
    if local == peer_uin || peer_uin == 0 {
        return Err(Error::Invalid("file transfer peer"));
    }
    let group = client.group().await?.ok_or(Error::NotPaired)?;
    if !group.devices.iter().any(|d| d.uin == local)
        || !group.devices.iter().any(|d| d.uin == peer_uin)
    {
        return Err(Error::NotPaired);
    }
    Ok(())
}
impl FileOffer {
    pub async fn issue(client: &Client, peer_uin: u64, lan: &[IpAddr]) -> Result<Self> {
        Self::create(client, peer_uin, lan, false).await
    }
    /// Requests a transfer invitation for an already paired device (command77).
    pub async fn invite(client: &Client, peer_uin: u64, lan: &[IpAddr]) -> Result<Self> {
        Self::create(client, peer_uin, lan, true).await
    }
    async fn create(client: &Client, peer_uin: u64, lan: &[IpAddr], notify: bool) -> Result<Self> {
        paired(client, peer_uin).await?;
        let identity = FileIdentity::generate().map_err(wx_ime_protocol::Error::from)?;
        let mut session = client.session().await?;
        let name = session.store.state.device_name.clone();
        let link = LinkInfo {
            lan: lan.to_vec(),
            device_name: name.clone(),
            preferred_client: Some(true),
        };
        let command = if notify { 77 } else { 73 };
        let response: wire::FileCodeResponse = if notify {
            session
                .api(
                    command,
                    wire::FileInviteRequest {
                        peer_uin: Some(peer_uin),
                        ca: Some(identity.ca().to_vec()),
                        link: Some(serde_json::to_vec(&link)?),
                        name: Some(name),
                        capabilities: Some(CAPABILITIES),
                    },
                )
                .await?
        } else {
            session
                .api(
                    command,
                    wire::FileCodeRequest {
                        ca: Some(identity.ca().to_vec()),
                        link: Some(serde_json::to_vec(&link)?),
                        name: Some(name),
                        capabilities: Some(CAPABILITIES),
                    },
                )
                .await?
        };
        business(command, response.status)?;
        let code = FileCode::parse(
            response
                .code
                .ok_or(Error::Invalid("file transfer code missing"))?,
        )?;
        let validity = response
            .expiration
            .filter(|n| *n > 0)
            .ok_or(Error::Invalid("file transfer expiry missing"))?;
        Ok(Self {
            client: client.clone(),
            peer_uin,
            identity,
            code,
            deadline: Instant::now() + Duration::from_millis(validity.min(30 * 60 * 1000)),
        })
    }
    pub fn code(&self) -> &FileCode {
        &self.code
    }
    pub fn identity(&self) -> &FileIdentity {
        &self.identity
    }
    pub async fn peer(&self) -> Result<Option<FilePeer>> {
        if Instant::now() >= self.deadline {
            return Err(Error::Timeout("file transfer rendezvous"));
        }
        paired(&self.client, self.peer_uin).await?;
        let response: wire::FilePeerResponse = self
            .client
            .session()
            .await?
            .api(
                75,
                wire::FilePeerRequest {
                    code: Some(self.code.expose().to_owned()),
                },
            )
            .await?;
        business(75, response.status)?;
        response
            .peer
            .map(|p| FilePeer::decode(p, self.peer_uin))
            .transpose()
    }
}
pub async fn join(
    client: &Client,
    peer_uin: u64,
    code: &FileCode,
    identity: &FileIdentity,
    lan: &[IpAddr],
    addresses: &[SocketAddr],
) -> Result<FilePeer> {
    paired(client, peer_uin).await?;
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|a| a.port() == 0 || a.ip().is_unspecified())
    {
        return Err(Error::Invalid("advertised gRPC address"));
    }
    let mut session = client.session().await?;
    let name = session.store.state.device_name.clone();
    let link = LinkInfo {
        lan: lan.to_vec(),
        device_name: name.clone(),
        preferred_client: Some(false),
    };
    let grpc = GrpcInfo {
        platform: 1,
        server_name: identity.server_name().to_owned(),
        device_name: name,
        available_nets: AvailableNets {
            lan: addresses
                .iter()
                .map(|a| LanAddress {
                    ip: a.ip(),
                    port: a.port(),
                })
                .collect(),
        },
    };
    let response: wire::FilePeerResponse = session
        .api(
            74,
            wire::FileJoinRequest {
                code: Some(code.expose().to_owned()),
                ca: Some(identity.ca().to_vec()),
                link: Some(serde_json::to_vec(&link)?),
                grpc: Some(serde_json::to_vec(&grpc)?),
                direct: Some(b"{}".to_vec()),
                state: Some(wire::FilePeerState::None as i32),
                capabilities: Some(CAPABILITIES),
            },
        )
        .await?;
    business(74, response.status)?;
    FilePeer::decode(
        response.peer.ok_or(Error::Invalid("file issuer missing"))?,
        peer_uin,
    )
}
