use crate::{Client, Error, Group, PairingCode, Result, error::business};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::time::Instant;
use wx_ime_protocol::{pairing::BindState, transport, wire};

#[derive(Debug, Clone, serde::Serialize)]
pub struct PairingPeer {
    pub name: String,
    pub platform: u32,
    /// Join responses contain only name/platform. Same-named group members are
    /// ambiguous; the SDK never invents a Uin in that case.
    pub uin: Option<u64>,
}
#[derive(Debug, Clone)]
pub enum PairingUpdate {
    AwaitingPeer,
    ConfirmationRequired {
        peer_name: String,
        peer_platform: u32,
    },
    Paired {
        group: Group,
        peer: PairingPeer,
    },
    CodeUnavailable,
    Expired,
    Cancelled,
}
struct ExpectedPeer {
    name: String,
    platform: u32,
}
struct PendingBind {
    id: String,
    peer: ExpectedPeer,
}
enum Role {
    Issuer {
        code: PairingCode,
        pending: Option<PendingBind>,
    },
    Joiner(PendingBind),
}
struct Lease(Client);
impl Lease {
    fn acquire(client: &Client) -> Result<Self> {
        client
            .inner
            .pairing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| Error::PairingInUse)?;
        Ok(Self(client.clone()))
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.inner.pairing.store(false, Ordering::Release);
    }
}

/// One live pairing transaction per Client. Drop releases the local lease;
/// call cancel explicitly to release the remote operation before its expiry.
pub struct PairingSession {
    lease: Lease,
    role: Role,
    deadline: Instant,
    ready: Option<(Group, PairingPeer)>,
    cancelled: bool,
}
impl Client {
    pub async fn request_pairing_code(&self) -> Result<PairingSession> {
        let lease = Lease::acquire(self)?;
        let code = self.issue_code().await?;
        Ok(PairingSession {
            lease,
            role: Role::Issuer {
                code,
                pending: None,
            },
            deadline: Instant::now() + Duration::from_secs(60),
            ready: None,
            cancelled: false,
        })
    }
    pub async fn join(&self, code: PairingCode) -> Result<PairingSession> {
        let lease = Lease::acquire(self)?;
        let mut session = self.session().await?;
        let request = wire::BindRequest {
            uin: Some(session.store.state.uin),
            platform: Some(1),
            code: Some(code.expose().into()),
            device: Some(wire::Device {
                name: Some(session.store.state.device_name.clone()),
                platform: Some(1),
            }),
        };
        let response: wire::BindResponse = session.api(transport::JOIN, request).await?;
        business(transport::JOIN, response.status)?;
        let id = response
            .bind_id
            .filter(|s| !s.is_empty())
            .ok_or(Error::Invalid("missing bind transaction ID"))?;
        let peer = expected(response.device)?;
        Ok(PairingSession {
            lease,
            role: Role::Joiner(PendingBind { id, peer }),
            deadline: Instant::now() + Duration::from_secs(60),
            ready: None,
            cancelled: false,
        })
    }
}
fn expected(device: Option<wire::Device>) -> Result<ExpectedPeer> {
    let device = device.ok_or(Error::Invalid("missing pairing peer"))?;
    Ok(ExpectedPeer {
        name: device
            .name
            .filter(|s| !s.is_empty())
            .ok_or(Error::Invalid("missing peer name"))?,
        platform: device
            .platform
            .ok_or(Error::Invalid("missing peer platform"))?,
    })
}
fn verified_peer(
    group: &Group,
    own: u64,
    expected: &ExpectedPeer,
    peer_uin: Option<u64>,
) -> Result<PairingPeer> {
    if !group.devices.iter().any(|d| d.uin == own) {
        return Err(Error::Invalid("completed group missing local member"));
    }
    let mut peers = group.devices.iter().filter(|d| {
        d.uin != own
            && d.name == expected.name
            && d.platform == expected.platform
            && peer_uin.is_none_or(|uin| d.uin == uin)
    });
    let peer = peers
        .next()
        .ok_or(Error::Invalid("completed group missing peer"))?;
    let uin = if peers.next().is_some() {
        None
    } else {
        Some(peer.uin)
    };
    Ok(PairingPeer {
        name: expected.name.clone(),
        platform: expected.platform,
        uin,
    })
}
impl PairingSession {
    pub fn code(&self) -> Option<&PairingCode> {
        match &self.role {
            Role::Issuer { code, .. } => Some(code),
            _ => None,
        }
    }
    pub async fn next_update(&mut self) -> Result<PairingUpdate> {
        if let Some((group, peer)) = &self.ready {
            return Ok(PairingUpdate::Paired {
                group: group.clone(),
                peer: peer.clone(),
            });
        }
        if self.cancelled {
            return Ok(PairingUpdate::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Ok(PairingUpdate::Expired);
        }
        let mut session = self.lease.0.session().await?;
        let own = session.store.state.uin;
        match &mut self.role {
            Role::Issuer { code, pending } => {
                let request = wire::PollCodeRequest {
                    uin: Some(own),
                    platform: Some(1),
                    code: Some(code.expose().into()),
                };
                let response: wire::PollCodeResponse =
                    session.api(transport::POLL_CODE, request).await?;
                match response.state {
                    Some(0) => {
                        *pending = None;
                        Ok(PairingUpdate::AwaitingPeer)
                    }
                    Some(1) => {
                        let peer = expected(response.device)?;
                        let id = response
                            .bind_id
                            .filter(|s| !s.is_empty())
                            .ok_or(Error::Invalid("missing pending bind ID"))?;
                        let update = PairingUpdate::ConfirmationRequired {
                            peer_name: peer.name.clone(),
                            peer_platform: peer.platform,
                        };
                        *pending = Some(PendingBind { id, peer });
                        Ok(update)
                    }
                    Some(2) => {
                        *pending = None;
                        Ok(PairingUpdate::CodeUnavailable)
                    }
                    _ => Err(Error::Invalid("unknown issuer pairing state")),
                }
            }
            Role::Joiner(binding) => {
                let request = wire::FetchBindRequest {
                    uin: Some(own),
                    platform: Some(1),
                    bind_id: Some(binding.id.clone()),
                };
                let response: wire::FetchBindResponse =
                    session.api(transport::FETCH_BIND, request).await?;
                business(transport::FETCH_BIND, response.status)?;
                match BindState::from(
                    response
                        .state
                        .ok_or(Error::Invalid("missing bind transaction state"))?,
                ) {
                    BindState::Pending => Ok(PairingUpdate::AwaitingPeer),
                    BindState::Complete => {
                        let group =
                            Group::try_from(&session.group(1).await?.ok_or(Error::NotPaired)?)?;
                        let peer = verified_peer(&group, own, &binding.peer, None)?;
                        self.ready = Some((group.clone(), peer.clone()));
                        Ok(PairingUpdate::Paired { group, peer })
                    }
                    BindState::Cancelled => {
                        self.cancelled = true;
                        Ok(PairingUpdate::Cancelled)
                    }
                    BindState::Unknown(state) => Err(Error::UnknownBindState(state)),
                }
            }
        }
    }
    /// Explicitly approve the peer exposed by the latest ConfirmationRequired update.
    pub async fn confirm(&mut self) -> Result<(Group, PairingPeer)> {
        if self.cancelled || self.ready.is_some() || Instant::now() >= self.deadline {
            return Err(Error::Invalid("pairing is no longer pending"));
        }
        let Role::Issuer {
            pending: Some(binding),
            ..
        } = &self.role
        else {
            return Err(Error::Invalid("no issuer confirmation request"));
        };
        let mut session = self.lease.0.session().await?;
        let own = session.store.state.uin;
        let request = wire::FetchBindRequest {
            uin: Some(own),
            platform: Some(1),
            bind_id: Some(binding.id.clone()),
        };
        let response: wire::ConfirmBindResponse =
            session.api(transport::CONFIRM_BIND, request).await?;
        business(transport::CONFIRM_BIND, response.status)?;
        let peer_uin = response
            .peer
            .as_ref()
            .and_then(|peer| peer.uin)
            .filter(|uin| *uin > 0)
            .ok_or(Error::Invalid("confirmation peer identity missing"))?;
        let group = Group::try_from(&session.group(1).await?.ok_or(Error::NotPaired)?)?;
        if response.group_id != Some(group.id) {
            return Err(Error::Invalid("confirmation group differs from membership"));
        }
        let peer = verified_peer(&group, own, &binding.peer, Some(peer_uin))?;
        self.ready = Some((group.clone(), peer.clone()));
        Ok((group, peer))
    }
    /// Release an issued code (field3) or join transaction (field4), then verify state.
    pub async fn cancel(&mut self) -> Result<()> {
        if self.cancelled {
            return Ok(());
        }
        if self.ready.is_some() {
            return Err(Error::Invalid("completed pairing cannot be cancelled"));
        }
        let mut session = self.lease.0.session().await?;
        let own = session.store.state.uin;
        let request = match &self.role {
            Role::Issuer { code, .. } => wire::ReleaseBindRequest {
                uin: Some(own),
                platform: Some(1),
                code: Some(code.expose().into()),
                bind_id: None,
            },
            Role::Joiner(binding) => wire::ReleaseBindRequest {
                uin: Some(own),
                platform: Some(1),
                code: None,
                bind_id: Some(binding.id.clone()),
            },
        };
        let _: wire::EmptyResponse = session.api(transport::RELEASE_BIND, request).await?;
        match &self.role {
            Role::Issuer { code, .. } => {
                let response: wire::PollCodeResponse = session
                    .api(
                        transport::POLL_CODE,
                        wire::PollCodeRequest {
                            uin: Some(own),
                            platform: Some(1),
                            code: Some(code.expose().into()),
                        },
                    )
                    .await?;
                if response.state != Some(2) {
                    return Err(Error::Invalid("code release not corroborated"));
                }
            }
            Role::Joiner(binding) => {
                let response: wire::FetchBindResponse = session
                    .api(
                        transport::FETCH_BIND,
                        wire::FetchBindRequest {
                            uin: Some(own),
                            platform: Some(1),
                            bind_id: Some(binding.id.clone()),
                        },
                    )
                    .await?;
                business(transport::FETCH_BIND, response.status)?;
                if response.state != Some(2) {
                    return Err(Error::Invalid("join cancellation not corroborated"));
                }
            }
        }
        self.cancelled = true;
        Ok(())
    }
    pub async fn wait(&mut self) -> Result<(Group, PairingPeer)> {
        loop {
            match self.next_update().await? {
                PairingUpdate::Paired { group, peer } => return Ok((group, peer)),
                PairingUpdate::ConfirmationRequired { .. } => {
                    return Err(Error::ConfirmationRequired);
                }
                PairingUpdate::Expired => {
                    return Err(Error::Timeout("confirmed pairing membership"));
                }
                PairingUpdate::CodeUnavailable => return Err(Error::CodeUnavailable),
                PairingUpdate::Cancelled => return Err(Error::Invalid("pairing was cancelled")),
                PairingUpdate::AwaitingPeer => {}
            }
            tokio::time::sleep_until((Instant::now() + Duration::from_secs(2)).min(self.deadline))
                .await;
        }
    }
}
