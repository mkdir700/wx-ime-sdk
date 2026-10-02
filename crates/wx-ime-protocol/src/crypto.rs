use crate::{Error, Result};
use openssl::{
    bn::BigNumContext,
    derive::Deriver,
    ec::{EcGroup, EcKey, EcPoint, PointConversionForm},
    hash::{MessageDigest, hash},
    nid::Nid,
    pkey::{PKey, Private},
    symm::{Cipher, decrypt, encrypt},
};
use zeroize::Zeroizing;

/// Control AES key. Debug intentionally hides its value.
#[derive(Clone)]
pub struct ControlKey(Zeroizing<Vec<u8>>);
impl std::fmt::Debug for ControlKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ControlKey([REDACTED])")
    }
}
impl ControlKey {
    pub fn bootstrap() -> Self {
        Self(Zeroizing::new(b"G7E1C8B0D7D7E4L3J2U9D6W0B2M8Q6O5".to_vec()))
    }
    pub fn seal(&self, plain: &[u8]) -> Result<Vec<u8>> {
        Ok(encrypt(Cipher::aes_256_ecb(), &self.0, None, plain)?)
    }
    pub fn open(&self, cipher: &[u8]) -> Result<Vec<u8>> {
        Ok(decrypt(Cipher::aes_256_ecb(), &self.0, None, cipher)?)
    }
    pub fn suffix(&self) -> &str {
        std::str::from_utf8(&self.0[28..]).expect("key is ASCII hex or bootstrap ASCII")
    }
}

/// ECDH identity. Key generation/import uses a mature OpenSSL backend.
pub struct IdentityKey(PKey<Private>);
impl std::fmt::Debug for IdentityKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("IdentityKey([REDACTED])")
    }
}
impl IdentityKey {
    pub fn generate() -> Result<Self> {
        let group = EcGroup::from_curve_name(Nid::SECP128R1)?;
        Ok(Self(PKey::from_ec_key(EcKey::generate(&group)?)?))
    }
    pub fn from_pem(pem: &[u8]) -> Result<Self> {
        let key = PKey::private_key_from_pem(pem)?;
        let ec = key.ec_key()?;
        if ec.group().curve_name() != Some(Nid::SECP128R1) {
            return Err(Error::Invalid("identity curve"));
        }
        ec.check_key()?;
        Ok(Self(key))
    }
    pub fn to_pem(&self) -> Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(self.0.private_key_to_pem_pkcs8()?))
    }
    pub fn public_hex(&self) -> Result<String> {
        let ec = self.0.ec_key()?;
        let mut context = BigNumContext::new()?;
        Ok(hex::encode_upper(ec.public_key().to_bytes(
            ec.group(),
            PointConversionForm::UNCOMPRESSED,
            &mut context,
        )?))
    }
    pub fn agree(&self, public_hex: &str) -> Result<ControlKey> {
        let group = EcGroup::from_curve_name(Nid::SECP128R1)?;
        let bytes = hex::decode(public_hex).map_err(|_| Error::Invalid("peer public key hex"))?;
        if bytes.len() != 33 || bytes[0] != 4 {
            return Err(Error::Invalid("peer public key encoding"));
        }
        let mut context = BigNumContext::new()?;
        let point = EcPoint::from_bytes(&group, &bytes, &mut context)?;
        let peer = EcKey::from_public_key(&group, &point)?;
        peer.check_key()?;
        let peer = PKey::from_ec_key(peer)?;
        let mut deriver = Deriver::new(&self.0)?;
        deriver.set_peer(&peer)?;
        let secret = Zeroizing::new(deriver.derive_to_vec()?);
        if secret.len() != 16 {
            return Err(Error::Invalid("ECDH shared X length"));
        }
        Ok(ControlKey(Zeroizing::new(
            hex::encode_upper(&*secret).into_bytes(),
        )))
    }
}

/// Image-object key, distinct from the control-session key. IV is the same 16 bytes.
#[derive(Clone)]
pub struct ImageKey(Zeroizing<[u8; 16]>);
impl std::fmt::Debug for ImageKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ImageKey([REDACTED])")
    }
}
impl ImageKey {
    pub fn new(bytes: [u8; 16]) -> Self {
        Self(Zeroizing::new(bytes))
    }
    pub fn bytes(&self) -> &[u8; 16] {
        &self.0
    }
    pub fn apply(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(encrypt(
            Cipher::aes_128_ctr(),
            self.0.as_ref(),
            Some(self.0.as_ref()),
            bytes,
        )?)
    }
}
pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(openssl::sha::sha256(bytes))
}
pub fn md5(bytes: &[u8]) -> Result<String> {
    Ok(hex::encode(hash(MessageDigest::md5(), bytes)?))
}
