//! `wxc`: command line client for the WeType cross-device clipboard.
mod args;
mod classify;
mod commands;
mod fsutil;
mod net;

use args::{Cli, Command};
use clap::Parser;
use classify::{Payload, classify, select_peer};
use commands::Result;
use std::{io::Read, time::Duration};
use wx_ime_sdk::{Client, ClientOptions};

fn read_stdin() -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    let limit = Duration::from_secs(cli.timeout);
    let state = args::state_dir(cli.state, |key| std::env::var(key).ok())
        .ok_or("cannot determine the state directory; pass --state DIR")?;

    // Validate everything that does not need the network before touching the identity.
    let payload = match &cli.command {
        Command::Send(send) => Some(classify(&send.input, send.text, read_stdin)?),
        _ => None,
    };
    if let Command::Pair { code } = &cli.command {
        wx_ime_sdk::PairingCode::parse(code)?;
    }

    commands::prepare_state(&state)?;
    let client = tokio::time::timeout(
        limit,
        Client::open(ClientOptions::new(&state, cli.name.clone())),
    )
    .await
    .map_err(|_| "timed out opening the client")??;

    let result = execute(&client, &cli.command, payload, &state, limit, cli.json).await;
    // Release SDK resources on success, error and timeout alike.
    let shutdown = client.shutdown().await;
    result?;
    shutdown?;
    Ok(())
}

async fn execute(
    client: &Client,
    command: &Command,
    payload: Option<Payload>,
    state: &std::path::Path,
    limit: Duration,
    json: bool,
) -> Result<()> {
    match command {
        Command::Pair { code } => commands::pair(client, code, json).await,
        Command::Devices => commands::devices(client, json).await,
        Command::Send(send) => match payload.expect("classified before opening") {
            Payload::Text(text) => commands::send_text(client, &text, limit, json).await,
            Payload::Image(path) => commands::send_image(client, &path, limit, json).await,
            Payload::File(path) => {
                let own = client.local_device().await?.uin;
                let peer = select_peer(&client.devices().await?, own, send.peer)?;
                let ip = match send.ip {
                    Some(ip) => net::validate_ip(ip)?,
                    None => net::local_ip()?,
                };
                commands::send_file(client, &path, peer, ip, limit, json).await
            }
        },
        Command::Recv(recv) if recv.file => commands::recv_files(client, state, recv, json).await,
        Command::Recv(recv) => commands::recv_clipboard(client, &recv.output, limit, json).await,
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("wxc: {error}");
        std::process::exit(1);
    }
}
