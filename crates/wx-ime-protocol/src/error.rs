#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid protocol value: {0}")]
    Invalid(&'static str),
    #[error("cryptographic operation failed")]
    Crypto(#[from] openssl::error::ErrorStack),
    #[error("protobuf decoding failed")]
    Protobuf(#[from] prost::DecodeError),
    #[error("response decompression failed")]
    Compression(#[from] snap::Error),
    #[error("object reference JSON invalid")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;
