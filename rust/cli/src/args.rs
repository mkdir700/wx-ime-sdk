//! Command line definition and state-directory resolution.
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

pub const DEFAULT_NAME_PREFIX: &str = "WXC-";
/// Name used by identities created before the name was remembered in the state directory.
pub const LEGACY_DEVICE_NAME: &str = "wxc";

#[derive(Debug, Parser)]
#[command(
    name = "wxc",
    version,
    about = "WeType (WeChat IME) cross-device clipboard client",
    long_about = "Pair with the official WeType input method, then send and receive \
                  text, images and files. Keep the same state directory and device \
                  name between runs: they identify this device."
)]
pub struct Cli {
    /// Identity directory [default: $WXC_STATE, else <config dir>/wxc]
    #[arg(long, global = true, env = "WXC_STATE", value_name = "DIR")]
    pub state: Option<PathBuf>,
    /// Device name shown in the official app. Set it when pairing; it is remembered in the
    /// state directory afterwards [default: WXC-<4 random characters>]
    #[arg(long, global = true, value_name = "NAME")]
    pub name: Option<String>,
    /// Seconds allowed for opening the client and for non-transfer operations
    #[arg(
        long,
        global = true,
        default_value_t = 120,
        value_parser = clap::value_parser!(u64).range(1..=86400),
        value_name = "SECONDS"
    )]
    pub timeout: u64,
    /// Print machine-readable JSON instead of plain text
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Join the six-digit pairing code shown by the official input method
    Pair {
        #[arg(value_name = "CODE")]
        code: String,
        /// Start over with a fresh identity: the current state directory is kept as a
        /// backup (restored if pairing fails). Use this to join a different official device
        /// when this identity is already paired
        #[arg(long)]
        reset: bool,
    },
    /// Show this device and the paired devices (Uin)
    Devices,
    /// Send text, an image or a file to the paired official device
    Send(SendArgs),
    /// Pull the latest clipboard content once, or accept a file invitation
    Recv(RecvArgs),
}

#[derive(Debug, Args)]
pub struct SendArgs {
    /// Text, a path to an image or file, or '-' to read text from stdin
    #[arg(value_name = "TEXT_OR_PATH")]
    pub input: String,
    /// Always send INPUT as text, even if a file with that name exists
    #[arg(long)]
    pub text: bool,
    /// Target device Uin for file transfer (see `wxc devices`)
    #[arg(long, value_name = "UIN")]
    pub peer: Option<u64>,
    /// Local LAN IP the peer can reach, for file transfer [default: auto-detect]
    #[arg(long, value_name = "IP")]
    pub ip: Option<std::net::IpAddr>,
}

#[derive(Debug, Args)]
pub struct RecvArgs {
    /// File mode: wait for a file invitation, accept it and save the file
    #[arg(short = 'f', long)]
    pub file: bool,
    /// Directory for received files and images
    #[arg(short = 'o', long, value_name = "DIR", default_value = ".")]
    pub output: PathBuf,
    /// File mode: exit after receiving this many files
    #[arg(
        short = 'n',
        long,
        default_value_t = 1,
        value_parser = clap::value_parser!(u32).range(1..),
        value_name = "N"
    )]
    pub count: u32,
    /// File mode: seconds to wait for an invitation, 0 waits until Ctrl-C
    #[arg(long, default_value_t = 300, value_name = "SECONDS")]
    pub wait: u64,
    /// File mode: local LAN IP the peer can reach [default: auto-detect]
    #[arg(long, value_name = "IP")]
    pub ip: Option<std::net::IpAddr>,
}

/// Explicit `--name`, else the name remembered in the state directory. An identity created
/// before names were remembered keeps the old default; a brand-new one gets `WXC-XXXX`.
pub fn device_name(
    explicit: Option<String>,
    remembered: Option<String>,
    identity_exists: bool,
    random: impl FnOnce() -> String,
) -> String {
    explicit
        .or_else(|| {
            remembered
                .map(|n| n.trim().to_owned())
                .filter(|n| !n.is_empty())
        })
        .unwrap_or_else(|| {
            if identity_exists {
                LEGACY_DEVICE_NAME.to_owned()
            } else {
                format!("{DEFAULT_NAME_PREFIX}{}", random())
            }
        })
}

/// Four random uppercase letters/digits for the default device name.
pub fn random_suffix() -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut value = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (0..4)
        .map(|_| {
            let c = ALPHABET[(value % ALPHABET.len() as u64) as usize] as char;
            value /= ALPHABET.len() as u64;
            c
        })
        .collect()
}

/// Resolution order: explicit flag/env, `$XDG_CONFIG_HOME/wxc`, `$HOME/.config/wxc`,
/// `%APPDATA%\wxc`.
pub fn state_dir(
    explicit: Option<PathBuf>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let non_empty = |key: &str| env(key).filter(|v| !v.is_empty());
    explicit
        .or_else(|| non_empty("XDG_CONFIG_HOME").map(|d| PathBuf::from(d).join("wxc")))
        .or_else(|| non_empty("HOME").map(|d| PathBuf::from(d).join(".config").join("wxc")))
        .or_else(|| non_empty("APPDATA").map(|d| PathBuf::from(d).join("wxc")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("wxc").chain(args.iter().copied()))
    }

    #[test]
    fn pair_takes_one_code() {
        let cli = parse(&["pair", "123456"]).unwrap();
        assert!(
            matches!(cli.command, Command::Pair { ref code, reset: false } if code == "123456")
        );
        assert!(parse(&["pair"]).is_err());
        assert!(matches!(
            parse(&["pair", "--reset", "123456"]).unwrap().command,
            Command::Pair { reset: true, .. }
        ));
    }

    #[test]
    fn send_flags() {
        let cli = parse(&["send", "--text", "./a.png"]).unwrap();
        let Command::Send(send) = cli.command else {
            panic!()
        };
        assert!(send.text && send.peer.is_none() && send.input == "./a.png");
        let cli = parse(&["send", "f.pdf", "--peer", "42", "--ip", "192.168.1.2"]).unwrap();
        let Command::Send(send) = cli.command else {
            panic!()
        };
        assert_eq!(send.peer, Some(42));
        assert_eq!(send.ip, Some("192.168.1.2".parse().unwrap()));
        assert!(parse(&["send"]).is_err());
        assert!(parse(&["send", "x", "--peer", "abc"]).is_err());
    }

    #[test]
    fn recv_defaults_and_file_mode() {
        let Command::Recv(recv) = parse(&["recv"]).unwrap().command else {
            panic!()
        };
        assert!(!recv.file && recv.output == Path::new(".") && recv.count == 1);
        let Command::Recv(recv) = parse(&["recv", "-f", "-o", "/tmp/x", "-n", "3"])
            .unwrap()
            .command
        else {
            panic!()
        };
        assert!(recv.file && recv.output == Path::new("/tmp/x") && recv.count == 3);
        assert!(parse(&["recv", "-n", "0"]).is_err());
    }

    #[test]
    fn global_options_apply_after_subcommand() {
        let cli = parse(&[
            "send",
            "hi",
            "--state",
            "/s",
            "--name",
            "n",
            "--timeout",
            "5",
        ])
        .unwrap();
        assert_eq!(cli.state, Some(PathBuf::from("/s")));
        assert_eq!((cli.name.as_deref(), cli.timeout), (Some("n"), 5));
        assert!(parse(&["--timeout", "0", "devices"]).is_err());
    }

    #[test]
    fn device_name_precedence() {
        let some = |s: &str| Some(s.to_owned());
        let rnd = || "AB12".to_owned();
        assert_eq!(device_name(some("a"), some("b"), true, rnd), "a");
        assert_eq!(device_name(None, some(" b\n"), true, rnd), "b");
        assert_eq!(device_name(None, some("  "), false, rnd), "WXC-AB12");
        assert_eq!(device_name(None, None, false, rnd), "WXC-AB12");
        assert_eq!(device_name(None, None, true, rnd), "wxc");
    }

    #[test]
    fn random_suffix_shape() {
        let suffix = random_suffix();
        assert_eq!(suffix.len(), 4);
        assert!(
            suffix
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        );
    }

    #[test]
    fn state_dir_precedence() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            state_dir(Some("/x".into()), env(&[("HOME", "/h")])),
            Some("/x".into())
        );
        assert_eq!(
            state_dir(None, env(&[("XDG_CONFIG_HOME", "/c"), ("HOME", "/h")])),
            Some("/c/wxc".into())
        );
        assert_eq!(
            state_dir(None, env(&[("HOME", "/h")])),
            Some("/h/.config/wxc".into())
        );
        assert_eq!(state_dir(None, env(&[("HOME", "")])), None);
    }
}
