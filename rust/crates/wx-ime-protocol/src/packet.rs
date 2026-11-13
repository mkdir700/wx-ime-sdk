use crate::{
    Error, Result,
    crypto::{ControlKey, md5, sha256},
    transport::{Endpoint, SIGN_SALT, VERSION},
    wire,
};
use prost::Message;
use std::collections::BTreeMap;

/// Values supplied by the network/session owner, not read from the environment.
pub struct RequestContext<'a> {
    pub timestamp_ms: i64,
    pub trace_id: &'a str,
    pub task: u32,
    pub uin: u64,
    pub registration_token: &'a str,
}
/// May contain short-lived credentials. Deliberately not Debug.
pub struct Packet {
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub fn encode(
    endpoint: Endpoint,
    plain: &[u8],
    key: Option<&ControlKey>,
    context: RequestContext<'_>,
) -> Result<Packet> {
    let body = match key {
        Some(key) => key.seal(plain)?,
        None => plain.to_vec(),
    };
    let mut headers = BTreeMap::new();
    for (name, value) in [
        ("Kb-OsType", "1".into()),
        ("Kb-Version", VERSION.into()),
        ("Kb-SystemVersion", "34".into()),
        ("Kb-PackageType", "2".into()),
        ("Kb-TimeStamp", context.timestamp_ms.to_string()),
        ("Kb-BodyMd5", md5(&body)?.to_uppercase()),
        ("Kb-TraceId", context.trace_id.into()),
        ("Kb-TaskId", context.task.to_string()),
        ("Kb-Scene", "1".into()),
        ("Kb-CompressionType", "1".into()),
        ("Content-Type", "application/octet-stream".into()),
        ("Use_DebugNet", "0".into()),
    ] {
        headers.insert(name.to_string(), value);
    }
    let mut order = vec!["Kb-OsType", "Kb-Version", "Kb-Scene"];
    match endpoint {
        Endpoint::Timestamp => {}
        Endpoint::Ecdh => {
            headers.insert("Kb-Uin".into(), context.uin.to_string());
            headers.insert("Kb-DeviceCodeRestrictionV2".into(), "1".into());
            order.extend(["Kb-TimeStamp", "Kb-BodyMd5", "Kb-TraceId", "Kb-TaskId"]);
        }
        Endpoint::Register => {
            headers.insert("Kb-GenUinToken".into(), context.registration_token.into());
            headers.insert("Kb-DeviceCodeRestrictionV2".into(), "1".into());
            order.extend([
                "Kb-TimeStamp",
                "Kb-BodyMd5",
                "Kb-GenUinToken",
                "Kb-TraceId",
                "Kb-TaskId",
            ]);
        }
        Endpoint::Api(command) => {
            headers.insert("Kb-Uin".into(), context.uin.to_string());
            headers.insert("Kb-CmdId".into(), "4814".into());
            headers.insert("Kb-SubCmdId".into(), command.to_string());
            headers.insert(
                "Kb-SharedKeySuffix".into(),
                key.ok_or(Error::Invalid("API session key missing"))?
                    .suffix()
                    .into(),
            );
            order.extend([
                "Kb-CmdId",
                "Kb-SubCmdId",
                "Kb-TimeStamp",
                "Kb-BodyMd5",
                "Kb-Uin",
                "Kb-TraceId",
                "Kb-TaskId",
            ]);
        }
    }
    if !matches!(endpoint, Endpoint::Timestamp) {
        let sign_input = order
            .iter()
            .map(|name| headers[*name].as_str())
            .collect::<String>()
            + SIGN_SALT;
        headers.insert(
            "Kb-Sign".into(),
            sha256(sign_input.as_bytes()).to_uppercase(),
        );
    }
    Ok(Packet { headers, body })
}

pub fn decode(
    cipher: &[u8],
    key: Option<&ControlKey>,
    compression: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if cipher.len() > max_bytes {
        return Err(Error::Invalid("control response size limit"));
    }
    let plain = match key {
        Some(key) => key.open(cipher)?,
        None => cipher.to_vec(),
    };
    match compression {
        "1" => Ok(plain),
        "2" => {
            if snap::raw::decompress_len(&plain)? > max_bytes {
                return Err(Error::Invalid("decompressed response size limit"));
            }
            Ok(snap::raw::Decoder::new().decompress_vec(&plain)?)
        }
        _ => Err(Error::Invalid("response compression type")),
    }
}

pub fn envelope(
    uin: u64,
    command: u32,
    device_info: &str,
    timestamp_ms: i64,
    body: Vec<u8>,
) -> Vec<u8> {
    let items = [
        ("vid", uin.to_string()),
        ("app_ver", VERSION.into()),
        ("platform", "1".into()),
        ("device", device_info.into()),
        ("channelid", String::new()),
        ("client_timestamp", timestamp_ms.to_string()),
    ]
    .into_iter()
    .map(|(key, value)| wire::HeaderItem {
        key: Some(key.into()),
        value: Some(value),
    })
    .collect();
    wire::BaseRequest {
        uin: Some(uin),
        subcommand: Some(command),
        header: Some(wire::Header { items }),
        body: Some(body),
    }
    .encode_to_vec()
}

pub fn business_body(bytes: &[u8]) -> Result<Vec<u8>> {
    wire::BaseResponse::decode(bytes)?
        .body
        .ok_or(Error::Invalid("business response body missing"))
}
