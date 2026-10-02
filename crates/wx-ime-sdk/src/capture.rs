//! Opt-in private research evidence. Never enabled implicitly.
use crate::{
    Result,
    storage::{private_dir, write_new},
};
use serde_json::json;
use std::path::PathBuf;
use wx_ime_protocol::{crypto::sha256, packet::Packet, transport::Endpoint};

pub(crate) struct Capture {
    directory: PathBuf,
    sequence: usize,
}
impl Capture {
    pub fn new(directory: PathBuf) -> Result<Self> {
        if directory.exists() {
            return Err(crate::Error::Invalid("capture directory must be new"));
        }
        private_dir(&directory)?;
        Ok(Self {
            directory,
            sequence: 0,
        })
    }
    pub fn request(&mut self, packet: &Packet) -> Result<usize> {
        self.sequence += 1;
        write_new(
            &self
                .directory
                .join(format!("{:04}.request.bin", self.sequence)),
            &packet.body,
        )?;
        write_new(
            &self
                .directory
                .join(format!("{:04}.headers.json", self.sequence)),
            &serde_json::to_vec_pretty(&packet.headers)?,
        )?;
        Ok(self.sequence)
    }
    pub fn response(
        &self,
        index: usize,
        endpoint: Endpoint,
        status: u16,
        compression: &str,
        cipher: &[u8],
        plain: &[u8],
    ) -> Result<()> {
        write_new(
            &self.directory.join(format!("{index:04}.response.bin")),
            cipher,
        )?;
        write_new(&self.directory.join(format!("{index:04}.plain.bin")), plain)?;
        write_new(
            &self.directory.join(format!("{index:04}.meta.json")),
            &serde_json::to_vec_pretty(
                &json!({"endpoint": endpoint.path(), "subcommand": if let Endpoint::Api(cmd)=endpoint {Some(cmd)} else {None}, "http_status": status, "compression": compression, "response_sha256": sha256(cipher), "plain_sha256": sha256(plain)}),
            )?,
        )?;
        Ok(())
    }
}
