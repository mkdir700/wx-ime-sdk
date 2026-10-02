//! Command implementations on top of the SDK. Network behavior is exercised by the
//! SDK's own end-to-end runs; the pure decisions live in `classify` and `fsutil`.
use crate::{
    args::RecvArgs,
    classify::sniff_image,
    fsutil::{commit_file, private_dir, write_unique},
    net,
};
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    path::Path,
    time::{Duration, Instant},
};
use tokio::time::{sleep, timeout};
use wx_ime_sdk::{
    Client, ClipboardContent, Error, FileReceiveOptions, FileTransferProgress, PairingCode,
};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// `--json` prints the machine-readable value; otherwise the human sentence.
fn report(json: bool, value: serde_json::Value, human: String) {
    if json {
        println!("{value}");
    } else {
        println!("{human}");
    }
}

const OUTCOME_UNKNOWN: &str =
    "operation timed out; delivery may be unknown. Check the peer before retrying";
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_POLL_FAILURES: u32 = 5;

pub async fn pair(client: &Client, code: &str, json: bool) -> Result<()> {
    let code = PairingCode::parse(code)?;
    let local = client.local_device().await?;
    eprintln!("Waiting for the official WeType to confirm pairing...");
    let mut pairing = client.join(code).await?;
    let result = pairing.wait().await;
    if result.is_err() {
        // Keep the original error if the remote cancellation also fails.
        let _ = pairing.cancel().await;
    }
    let (group, peer) = result?;
    let human = format!(
        "Paired with {}.\nThis device: {} (Uin {})",
        peer.name, local.name, local.uin
    );
    report(
        json,
        json!({"status": "paired", "local": local, "group": group, "peer": peer}),
        human,
    );
    Ok(())
}

pub async fn devices(client: &Client, json: bool) -> Result<()> {
    let local = client.local_device().await?;
    let group = client.group().await?;
    let mut human = format!("This device: {} (Uin {})", local.name, local.uin);
    match &group {
        Some(group) => {
            human.push_str(
                "
Paired devices:",
            );
            for device in group.devices.iter().filter(|d| d.uin != local.uin) {
                let files = if device.supports_file_transfer {
                    ""
                } else {
                    " [no file transfer]"
                };
                human.push_str(&format!(
                    "
  {}  {}{files}",
                    device.uin, device.name
                ));
            }
        }
        None => human.push_str(
            "
Not paired. Run `wxc pair CODE`.",
        ),
    }
    report(json, json!({"local": local, "group": group}), human);
    Ok(())
}

pub async fn send_text(client: &Client, text: &str, limit: Duration, json: bool) -> Result<()> {
    let receipt = timeout(limit, client.send_text(text))
        .await
        .map_err(|_| OUTCOME_UNKNOWN)??;
    report(
        json,
        json!({"status": "server_accepted", "kind": "text", "receipt": receipt}),
        "Text sent (accepted by the server).".into(),
    );
    Ok(())
}

pub async fn send_image(client: &Client, path: &Path, limit: Duration, json: bool) -> Result<()> {
    let receipt = timeout(limit, client.send_image(path))
        .await
        .map_err(|_| OUTCOME_UNKNOWN)??;
    report(
        json,
        json!({"status": "server_accepted", "kind": "image", "receipt": receipt}),
        "Image sent (accepted by the server).".into(),
    );
    Ok(())
}

/// `limit` bounds waiting for the peer to accept; the transfer itself is not time-limited.
pub async fn send_file(
    client: &Client,
    path: &Path,
    peer: u64,
    ip: std::net::IpAddr,
    limit: Duration,
    json: bool,
) -> Result<()> {
    let mut transfer = client.send_file(peer, path, &[ip]).await?;
    eprintln!("File invitation sent; waiting for device {peer} to accept...");
    let accepted = timeout(limit, async {
        loop {
            if matches!(transfer.progress(), FileTransferProgress::Sending { .. }) {
                return;
            }
            if !transfer.progress_changed().await {
                return;
            }
        }
    })
    .await;
    if accepted.is_err() {
        transfer.cancel();
        return Err("the peer did not accept the file in time".into());
    }
    let tty = std::io::stderr().is_terminal();
    while transfer.progress_changed().await {
        if let (
            true,
            FileTransferProgress::Sending {
                acknowledged_bytes,
                total_bytes,
                ..
            },
        ) = (tty, transfer.progress())
        {
            eprint!("\rsending {acknowledged_bytes}/{total_bytes} bytes");
        }
    }
    if tty {
        eprintln!();
    }
    let receipt = transfer.wait().await?;
    report(
        json,
        json!({"status": "range_acknowledged", "kind": "file", "peer_uin": peer,
               "receipt": {"id": receipt.id, "bytes": receipt.bytes}}),
        format!(
            "Sent {} to device {peer} ({} bytes).",
            path.display(),
            receipt.bytes
        ),
    );
    Ok(())
}

/// One-shot clipboard pull. Text goes to stdout; images are saved into `output`.
pub async fn recv_clipboard(
    client: &Client,
    output: &Path,
    limit: Duration,
    json: bool,
) -> Result<()> {
    let Some(received) = timeout(limit, client.poll_clipboard_once())
        .await
        .map_err(|_| "timed out polling the clipboard")??
    else {
        eprintln!("No new clipboard content.");
        return Ok(());
    };
    match &received.content {
        ClipboardContent::Text(text) => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(text.as_bytes())?;
            if stdout.is_terminal() && !text.ends_with('\n') {
                stdout.write_all(b"\n")?;
            }
            stdout.flush()?;
        }
        ClipboardContent::Image(image) => {
            // The SDK cache is recycled on acknowledge, so copy first.
            let bytes = std::fs::read(&image.path)?;
            let extension = sniff_image(&bytes).map_or("bin", |kind| kind.extension());
            let name = format!("wxc-{}.{extension}", received.id.version());
            let saved = write_unique(output, &name, &bytes)?;
            report(
                json,
                json!({"status": "saved", "kind": "image", "path": saved, "bytes": bytes.len()}),
                format!(
                    "Saved image to {} ({} bytes).",
                    saved.display(),
                    bytes.len()
                ),
            );
        }
    }
    client.acknowledge(received.id).await?;
    Ok(())
}

fn remember(path: &Path, code: &str) {
    let _ = std::fs::write(path, code);
}

/// Waits for a file invitation from a paired device, accepts it, and saves the files.
pub async fn recv_files(client: &Client, state: &Path, args: &RecvArgs, json: bool) -> Result<()> {
    let ip = match args.ip {
        Some(ip) => net::validate_ip(ip)?,
        None => net::local_ip()?,
    };
    let staging = state.join("incoming");
    private_dir(&staging)?;
    // The server keeps an invitation queryable after it was used; skip the last handled one.
    let handled_path = state.join("last-invite");
    let handled = std::fs::read_to_string(&handled_path).ok();
    let deadline = (args.wait > 0).then(|| Instant::now() + Duration::from_secs(args.wait));
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    eprintln!("Waiting for a file invitation on {ip} (Ctrl-C to stop)...");
    let mut failures = 0;
    loop {
        let invite = match client.pending_file_invite().await {
            Ok(Some(invite)) if handled.as_deref() != Some(invite.code.expose()) => {
                failures = 0;
                Some(invite)
            }
            Ok(_) => {
                failures = 0;
                None
            }
            Err(error) => {
                failures += 1;
                if failures >= MAX_POLL_FAILURES {
                    return Err(error.into());
                }
                eprintln!("warning: invitation query failed: {error}");
                None
            }
        };
        let Some(invite) = invite else {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return Err("timed out waiting for a file invitation".into());
            }
            tokio::select! {
                _ = &mut ctrl_c => return Err("interrupted".into()),
                () = sleep(POLL_INTERVAL) => {}
            }
            continue;
        };
        eprintln!(
            "Invitation from {} (Uin {}); connecting...",
            invite.sender_name, invite.sender_uin
        );
        let options = FileReceiveOptions {
            bind_address: "0.0.0.0:0".parse()?,
            advertised_ips: vec![ip],
            download_dir: staging.clone(),
        };
        let mut receiver = match client
            .receive_file(invite.sender_uin, &invite.code, options)
            .await
        {
            Ok(receiver) => receiver,
            Err(error) => {
                // Typically a stale invitation; remember it and keep waiting for a new one.
                eprintln!("warning: cannot accept this invitation: {error}");
                remember(&handled_path, invite.code.expose());
                continue;
            }
        };
        remember(&handled_path, invite.code.expose());
        let mut received = 0;
        while received < args.count {
            let outcome = tokio::select! {
                _ = &mut ctrl_c => return Err("interrupted".into()),
                outcome = receiver.receive() => outcome,
            };
            match outcome {
                Ok(file) => {
                    let name = file
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("file")
                        .to_owned();
                    let saved = commit_file(&file.path, &args.output, &name)?;
                    if let Some(dir) = file.path.parent() {
                        let _ = std::fs::remove_dir(dir);
                    }
                    report(
                        json,
                        json!({"status": "received", "kind": "file", "path": saved,
                               "bytes": file.bytes, "from_uin": invite.sender_uin}),
                        format!("Received {} ({} bytes).", saved.display(), file.bytes),
                    );
                    received += 1;
                }
                Err(Error::FilePeerDisconnected | Error::Closed) if received > 0 => break,
                Err(error) => return Err(error.into()),
            }
        }
        return Ok(());
    }
}

/// Opens the identity directory without leaving it world-readable.
pub fn prepare_state(state: &Path) -> Result<()> {
    private_dir(state)?;
    Ok(())
}
