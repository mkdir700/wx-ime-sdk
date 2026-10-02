//! HTTP control transport metadata. WebSocket framing is not yet verified.
pub const HOST: &str = "https://wetype.weixin.qq.com";
pub const VERSION: &str = "3.5.4.57423";
pub const PLATFORM: u32 = 1;
pub const SIGN_SALT: &str = "aI0wC6qT1nB3nN5r";

#[derive(Debug, Clone, Copy)]
pub enum Endpoint {
    Timestamp,
    Ecdh,
    Register,
    Api(u32),
}
impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Self::Timestamp => "/timestamp",
            Self::Ecdh => "/oauth_pubkey_v2",
            Self::Register => "/gen_uin_v2",
            Self::Api(_) => "/api_v2",
        }
    }
}
pub const REQUEST_CODE: u32 = 30;
pub const JOIN: u32 = 31;
pub const CONFIRM_BIND: u32 = 32;
pub const RELEASE_BIND: u32 = 33;
pub const GROUP: u32 = 34;
pub const SEND_CLIPBOARD: u32 = 36;
pub const FETCH_BIND: u32 = 38;
pub const POLL_CODE: u32 = 40;
pub const COS_CREDENTIALS: u32 = 60;
