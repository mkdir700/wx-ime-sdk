use crate::{Error, Result};

/// Exactly six ASCII digits; leading zeroes are preserved. Debug is redacted.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode(String);
impl PairingCode {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(Error::Invalid("pairing code must contain six ASCII digits"));
        }
        Ok(Self(value))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingCode([REDACTED])")
    }
}

/// Only meanings corroborated by transaction and membership captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindState {
    Pending,
    Complete,
    Cancelled,
    Unknown(u32),
}
impl From<u32> for BindState {
    fn from(value: u32) -> Self {
        match value {
            0 => Self::Pending,
            1 => Self::Complete,
            2 => Self::Cancelled,
            value => Self::Unknown(value),
        }
    }
}
