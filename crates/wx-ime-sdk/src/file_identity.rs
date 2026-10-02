//! Ephemeral independent TLS identity for a single file rendezvous.
use crate::file_transport::{ClientCredentials, ServerCredentials};
use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        X509, X509NameBuilder,
        extension::{
            AuthorityKeyIdentifier, BasicConstraints, ExtendedKeyUsage, KeyUsage,
            SubjectAlternativeName, SubjectKeyIdentifier,
        },
    },
};
use zeroize::Zeroizing;

type Result<T> = std::result::Result<T, openssl::error::ErrorStack>;
/// No Debug: retains ephemeral private TLS material until the transfer ends.
pub struct FileIdentity {
    ca: Vec<u8>,
    server_name: String,
    client_certificate: Vec<u8>,
    client_key: Zeroizing<Vec<u8>>,
    server_certificate: Vec<u8>,
    server_key: Zeroizing<Vec<u8>>,
}
impl FileIdentity {
    pub fn generate() -> Result<Self> {
        let root_key = key()?;
        let root = certificate("UniClipboard WeType private CA", &root_key, None, None)?;
        let server_name = format!("{}.wetype.private", uuid::Uuid::new_v4().simple());
        let client_key = key()?;
        let server_key = key()?;
        let client = certificate(
            "UniClipboard WeType client",
            &client_key,
            Some((&root, &root_key)),
            None,
        )?;
        let server = certificate(
            &server_name,
            &server_key,
            Some((&root, &root_key)),
            Some(&server_name),
        )?;
        Ok(Self {
            ca: root.to_pem()?,
            server_name,
            client_certificate: client.to_pem()?,
            client_key: client_key.private_key_to_pem_pkcs8()?.into(),
            server_certificate: server.to_pem()?,
            server_key: server_key.private_key_to_pem_pkcs8()?.into(),
        })
    }
    pub fn ca(&self) -> &[u8] {
        &self.ca
    }
    pub fn server_name(&self) -> &str {
        &self.server_name
    }
    pub fn server_credentials(&self, peer_ca: &[u8]) -> ServerCredentials {
        ServerCredentials {
            certificate: self.server_certificate.clone(),
            private_key: self.server_key.clone(),
            peer_ca: peer_ca.to_vec(),
        }
    }
    pub fn client_credentials(&self, peer_ca: &[u8], server_name: &str) -> ClientCredentials {
        ClientCredentials {
            certificate: self.client_certificate.clone(),
            private_key: self.client_key.clone(),
            peer_ca: peer_ca.to_vec(),
            server_name: server_name.to_owned(),
        }
    }
}
fn key() -> Result<PKey<Private>> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    PKey::from_ec_key(EcKey::generate(&group)?)
}
fn certificate(
    common_name: &str,
    key: &PKey<Private>,
    issuer: Option<(&X509, &PKey<Private>)>,
    dns_name: Option<&str>,
) -> Result<X509> {
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    let mut serial = [0u8; 16];
    openssl::rand::rand_bytes(&mut serial)?;
    serial[0] |= 1;
    let number = BigNum::from_slice(&serial)?;
    let serial = Asn1Integer::from_bn(&number)?;
    builder.set_serial_number(&serial)?;
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text("CN", common_name)?;
    let name = name.build();
    builder.set_subject_name(&name)?;
    builder.set_pubkey(key)?;
    let not_before = Asn1Time::days_from_now(0)?;
    let not_after = Asn1Time::days_from_now(2)?;
    builder.set_not_before(&not_before)?;
    builder.set_not_after(&not_after)?;
    if let Some((parent, _)) = issuer {
        builder.set_issuer_name(parent.subject_name())?;
    } else {
        builder.set_issuer_name(&name)?;
    }
    let constraints = if issuer.is_none() {
        BasicConstraints::new().critical().ca().pathlen(0).build()?
    } else {
        BasicConstraints::new().critical().build()?
    };
    builder.append_extension(constraints)?;
    let usage = if issuer.is_none() {
        KeyUsage::new()
            .critical()
            .key_cert_sign()
            .crl_sign()
            .build()?
    } else {
        KeyUsage::new().critical().digital_signature().build()?
    };
    builder.append_extension(usage)?;
    let subject_key = SubjectKeyIdentifier::new()
        .build(&builder.x509v3_context(issuer.map(|i| i.0.as_ref()), None))?;
    builder.append_extension(subject_key)?;
    if issuer.is_some() {
        let authority = AuthorityKeyIdentifier::new()
            .keyid(true)
            .build(&builder.x509v3_context(issuer.map(|i| i.0.as_ref()), None))?;
        builder.append_extension(authority)?;
        let usage = if dns_name.is_some() {
            ExtendedKeyUsage::new().server_auth().build()?
        } else {
            ExtendedKeyUsage::new().client_auth().build()?
        };
        builder.append_extension(usage)?;
    }
    if let Some(dns) = dns_name {
        let alternative = SubjectAlternativeName::new()
            .dns(dns)
            .build(&builder.x509v3_context(issuer.map(|i| i.0.as_ref()), None))?;
        builder.append_extension(alternative)?;
    }
    builder.sign(issuer.map(|i| i.1).unwrap_or(key), MessageDigest::sha256())?;
    Ok(builder.build())
}
