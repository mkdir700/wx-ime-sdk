//! Discovery of file invitations that a paired peer has issued to this device.
use crate::{Client, Error, FileCode, Group, Result};
use serde::Serialize;

/// GroupRequest.filter bits: bound list and pending transfer invite.
const FILTER_BOUND_LIST: u32 = 1;
const FILTER_PENDING_INVITE: u32 = 64;

/// A file invitation from a paired peer. Pass `code` and `sender_uin` to
/// [`Client::receive_file`] to accept it.
#[derive(Debug, Clone, Serialize)]
pub struct FileInvite {
    #[serde(skip)]
    pub code: FileCode,
    pub sender_uin: u64,
    pub sender_name: String,
    pub platform: u32,
    /// Raw server value; the unit is not verified, so it is not used for expiry.
    pub send_time: i64,
    pub capabilities: u64,
}

impl Client {
    /// Queries the server once for a pending file invitation addressed to this device.
    /// The invitation is not consumed; it stays queryable until the server expires it
    /// or it is accepted, so callers deduplicate by `code`.
    pub async fn pending_file_invite(&self) -> Result<Option<FileInvite>> {
        let mut session = self.session().await?;
        let own = session.store.state.uin;
        let Some(wire_group) = session
            .group(FILTER_BOUND_LIST | FILTER_PENDING_INVITE)
            .await?
        else {
            return Ok(None);
        };
        drop(session);
        let Some(invite) = wire_group.pending_invite.as_ref() else {
            return Ok(None);
        };
        let code = invite.transfer_code.as_deref().unwrap_or_default();
        if code.is_empty() {
            return Ok(None);
        }
        let sender_uin = invite
            .sender_uin
            .filter(|uin| *uin > 0)
            .ok_or(Error::Invalid("file invitation sender missing"))?;
        let group = Group::try_from(&wire_group)?;
        if sender_uin == own || !group.devices.iter().any(|d| d.uin == sender_uin) {
            return Err(Error::Invalid("file invitation sender is not a group peer"));
        }
        Ok(Some(FileInvite {
            code: FileCode::parse(code)?,
            sender_uin,
            sender_name: invite.device_name.clone().unwrap_or_default(),
            platform: invite.platform.unwrap_or(0),
            send_time: invite.send_time.unwrap_or(0),
            capabilities: invite.capabilities.unwrap_or(0),
        }))
    }
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use wx_ime_protocol::wire;

    #[test]
    fn group_decodes_pending_invite_field_seven() {
        // field 1 group_id=9, field 7 invite{1:"abc",2:"Pixel",3:42,4:1700,5:1,6:2}
        let mut invite = vec![0x0a, 3, b'a', b'b', b'c', 0x12, 5];
        invite.extend_from_slice(b"Pixel");
        invite.extend_from_slice(&[0x18, 42, 0x20, 0xa4, 0x0d, 0x28, 1, 0x30, 2]);
        let mut bytes = vec![0x08, 9, 0x3a, invite.len() as u8];
        bytes.extend_from_slice(&invite);
        let group = wire::Group::decode(bytes.as_slice()).unwrap();
        let invite = group.pending_invite.unwrap();
        assert_eq!(group.group_id, Some(9));
        assert_eq!(invite.transfer_code.as_deref(), Some("abc"));
        assert_eq!(invite.device_name.as_deref(), Some("Pixel"));
        assert_eq!(invite.sender_uin, Some(42));
        assert_eq!(invite.send_time, Some(1700));
        assert_eq!(invite.platform, Some(1));
        assert_eq!(invite.capabilities, Some(2));
    }

    #[test]
    fn group_request_encodes_transfer_code_as_field_six() {
        let request = wire::GroupRequest {
            current_transfer_code: Some("x".into()),
            ..Default::default()
        };
        assert_eq!(request.encode_to_vec(), [0x32, 1, b'x']);
    }
}
