//! Reproducible public test identities and protocol responses for the Go E2E.
//! Small public test scalars are constructed at runtime; no PEM key is committed.
use openssl::{
    bn::{BigNum, BigNumContext},
    ec::{EcGroup, EcKey, EcPoint},
    nid::Nid,
    pkey::PKey,
};
use prost::Message;
use serde_json::json;
use wx_ime_protocol::{
    crypto::{ControlKey, IdentityKey, md5, sha256},
    transport::SIGN_SALT,
    wire,
};

fn test_identity(scalar: u32) -> Result<IdentityKey, Box<dyn std::error::Error>> {
    let group = EcGroup::from_curve_name(Nid::SECP128R1)?;
    let private = BigNum::from_u32(scalar)?;
    let mut context = BigNumContext::new()?;
    let mut public = EcPoint::new(&group)?;
    public.mul_generator2(&group, &private, &mut context)?;
    let key = PKey::from_ec_key(EcKey::from_private_components(&group, &private, &public)?)?;
    Ok(IdentityKey::from_pem(&key.private_key_to_pem_pkcs8()?)?)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = test_identity(1)?;
    let server = test_identity(2)?;
    let public = client.public_hex()?;
    let key = server.agree(&public)?;
    let prefix = format!("AND{}000000000000", &"UniClipboard;research"[..17]);
    let code = format!(
        "{prefix}{}",
        md5(format!("{prefix}{SIGN_SALT}").as_bytes())?.to_uppercase()
    );
    let identity = json!({"format":1,"device_name":"Rust fixture","private_pem":"","device_code":code,"uuid":"00000000-0000-4000-8000-000000000001","uin":101,"server_key":"","consumed":{},"pending":null});
    let timestamp = wire::Timestamp {
        milliseconds: Some(1_700_000_000_000),
    }
    .encode_to_vec();
    let ecdh = wire::EcdhResponse {
        status: Some(0),
        public_key: Some(server.public_hex()?),
        ..Default::default()
    }
    .encode_to_vec();
    let devices = [(101, "Rust fixture"), (202, "Rust peer")]
        .into_iter()
        .map(|(uin, name)| wire::BoundInfo {
            uin: Some(uin),
            device: Some(wire::GroupDevice {
                name: Some(name.into()),
                platform: Some(1),
                ..Default::default()
            }),
        })
        .collect();
    let group = wire::GroupResponse {
        status: Some(0),
        group: Some(wire::Group {
            group_id: Some(9),
            devices,
            clipboard: Some(wire::ClipboardMsg {
                version: Some(7),
                r#type: Some(1),
                binary_data: Some(b"Rust fixture clipboard".to_vec()),
                expire_at: Some(1_700_003_600),
                from_uin: Some(202),
                trace_id: Some("rust-fixture".into()),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec();
    let envelope = wire::BaseResponse {
        body: Some(group),
        ..Default::default()
    }
    .encode_to_vec();
    let compressed = snap::raw::Encoder::new().compress_vec(&envelope)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"identity":identity,"timestamp":hex::encode(timestamp),"ecdh":hex::encode(ControlKey::bootstrap().seal(&ecdh)?),"group":hex::encode(key.seal(&compressed)?),"public_key_sha256":sha256(public.as_bytes())})
        )?
    );
    Ok(())
}
