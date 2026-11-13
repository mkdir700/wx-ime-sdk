use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use wx_ime_protocol::wire;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub uin: u64,
    pub name: String,
    pub platform: u32,
    pub supports_file_transfer: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: u64,
    pub devices: Vec<Device>,
}
impl TryFrom<&wire::Group> for Group {
    type Error = Error;
    fn try_from(group: &wire::Group) -> Result<Self> {
        let id = group
            .group_id
            .filter(|id| *id > 0)
            .ok_or(Error::Invalid("missing group ID"))?;
        let devices = group
            .devices
            .iter()
            .map(|bound| {
                let info = bound
                    .device
                    .as_ref()
                    .ok_or(Error::Invalid("missing device information"))?;
                Ok(Device {
                    uin: bound
                        .uin
                        .filter(|uin| *uin > 0)
                        .ok_or(Error::Invalid("missing device Uin"))?,
                    name: info
                        .name
                        .clone()
                        .ok_or(Error::Invalid("missing device name"))?,
                    platform: info
                        .platform
                        .ok_or(Error::Invalid("missing device platform"))?,
                    supports_file_transfer: info.support_file_transfer.unwrap_or(false),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { id, devices })
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct LocalDevice {
    pub uin: u64,
    pub name: String,
    pub public_key_sha256: String,
}
