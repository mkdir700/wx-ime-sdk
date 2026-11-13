//! Decides how `wxc send` interprets its argument, and which peer receives a file.
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use wx_ime_sdk::Device;

/// Largest encoded image the SDK accepts; bigger images travel as plain files.
pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
/// Largest plain file the SDK accepts.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum Payload {
    Text(String),
    Image(PathBuf),
    File(PathBuf),
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ImageKind {
    Png,
    Jpeg,
}
impl ImageKind {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
}

/// Recognises the encoded formats the official app renders as images, by content.
pub fn sniff_image(head: &[u8]) -> Option<ImageKind> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(ImageKind::Png)
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageKind::Jpeg)
    } else {
        None
    }
}

fn looks_like_path(value: &str) -> bool {
    value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value.starts_with('/')
        || value.starts_with(".\\")
        || value.starts_with("..\\")
}

/// Terminals paste drag-and-dropped paths as `a\ b.png`; inside quotes those backslashes
/// are literal. When the literal path is missing, retry with the escapes removed (Unix only).
fn resolve(input: &str) -> PathBuf {
    let literal = Path::new(input);
    if cfg!(unix) && input.contains('\\') && !literal.exists() {
        let mut unescaped = String::with_capacity(input.len());
        let mut chars = input.chars();
        while let Some(c) = chars.next() {
            match (c, c == '\\') {
                (_, true) => unescaped.extend(chars.next()),
                _ => unescaped.push(c),
            }
        }
        if Path::new(&unescaped).exists() {
            return PathBuf::from(unescaped);
        }
    }
    literal.to_owned()
}

/// `read_stdin` is only called for the `-` argument.
pub fn classify(
    input: &str,
    force_text: bool,
    read_stdin: impl FnOnce() -> std::io::Result<Vec<u8>>,
) -> Result<Payload, String> {
    if input == "-" {
        let bytes = read_stdin().map_err(|e| format!("cannot read stdin: {e}"))?;
        let text = String::from_utf8(bytes).map_err(|_| "stdin is not valid UTF-8".to_string())?;
        return Ok(Payload::Text(text));
    }
    if force_text {
        return Ok(Payload::Text(input.to_owned()));
    }
    let resolved = resolve(input);
    let path = resolved.as_path();
    match fs::metadata(path) {
        Ok(meta) if meta.is_file() => {
            if meta.len() == 0 {
                return Err(format!("{input} is empty"));
            }
            if meta.len() > MAX_FILE_BYTES {
                return Err(format!("{input} exceeds the 1 GiB file limit"));
            }
            let mut head = [0u8; 8];
            let mut file = fs::File::open(path).map_err(|e| format!("cannot open {input}: {e}"))?;
            let read = file
                .read(&mut head)
                .map_err(|e| format!("cannot read {input}: {e}"))?;
            if sniff_image(&head[..read]).is_some() && meta.len() <= MAX_IMAGE_BYTES {
                Ok(Payload::Image(path.to_owned()))
            } else {
                Ok(Payload::File(path.to_owned()))
            }
        }
        Ok(meta) if meta.is_dir() => {
            Err(format!("{input} is a directory; send a file or use --text"))
        }
        Ok(_) => Err(format!(
            "{input} is not a regular file; use --text to send it as text"
        )),
        Err(_) if looks_like_path(input) => Err(format!(
            "{input} looks like a path but does not exist; use --text to send it as text"
        )),
        Err(_) => Ok(Payload::Text(input.to_owned())),
    }
}

/// Picks the file-transfer target among paired devices.
pub fn select_peer(devices: &[Device], own: u64, requested: Option<u64>) -> Result<u64, String> {
    let others: Vec<&Device> = devices.iter().filter(|d| d.uin != own).collect();
    if let Some(uin) = requested {
        return match others.iter().find(|d| d.uin == uin) {
            Some(d) if d.supports_file_transfer => Ok(uin),
            Some(_) => Err(format!("device {uin} does not support file transfer")),
            None => Err(format!("device {uin} is not paired with this identity")),
        };
    }
    let capable: Vec<&&Device> = others.iter().filter(|d| d.supports_file_transfer).collect();
    match capable.as_slice() {
        [] => Err("no paired device supports file transfer; run `wxc pair` first".into()),
        [only] => Ok(only.uin),
        many => {
            let list: Vec<String> = many
                .iter()
                .map(|d| format!("{} ({})", d.uin, d.name))
                .collect();
            Err(format!(
                "several devices can receive files, choose one with --peer: {}",
                list.join(", ")
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn no_stdin() -> std::io::Result<Vec<u8>> {
        panic!("stdin must not be read")
    }
    fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
        let path = dir.join(name);
        fs::File::create(&path).unwrap().write_all(bytes).unwrap();
        path.to_str().unwrap().to_owned()
    }
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    const JPG: &[u8] = &[0xff, 0xd8, 0xff, 0xe0, 0, 0];

    #[test]
    fn sniffs_by_content_not_extension() {
        let dir = tempfile::tempdir().unwrap();
        let png = write(dir.path(), "fake.txt", PNG);
        assert_eq!(
            classify(&png, false, no_stdin),
            Ok(Payload::Image(png.clone().into()))
        );
        let jpg = write(dir.path(), "photo.bin", JPG);
        assert_eq!(
            classify(&jpg, false, no_stdin),
            Ok(Payload::Image(jpg.clone().into()))
        );
        let disguised = write(dir.path(), "real.png", b"not an image at all");
        assert_eq!(
            classify(&disguised, false, no_stdin),
            Ok(Payload::File(disguised.clone().into()))
        );
        let pdf = write(dir.path(), "a.pdf", b"%PDF-1.7 ....");
        assert!(matches!(
            classify(&pdf, false, no_stdin),
            Ok(Payload::File(_))
        ));
    }

    #[test]
    #[cfg(unix)]
    fn backslash_escaped_paths_are_unescaped() {
        let dir = tempfile::tempdir().unwrap();
        let real = write(dir.path(), "Image Sep 26, 01.png", PNG);
        let escaped = real.replace(' ', "\\ ");
        assert_eq!(
            classify(&escaped, false, no_stdin),
            Ok(Payload::Image(real.clone().into()))
        );
        let missing = format!("{}/no\\ such.png", dir.path().display());
        assert!(classify(&missing, false, no_stdin).is_err());
    }

    #[test]
    fn empty_file_and_directory_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write(dir.path(), "empty", b"");
        assert!(
            classify(&empty, false, no_stdin)
                .unwrap_err()
                .contains("empty")
        );
        assert!(
            classify(dir.path().to_str().unwrap(), false, no_stdin)
                .unwrap_err()
                .contains("directory")
        );
    }

    #[test]
    fn text_rules() {
        assert_eq!(
            classify("1111", false, no_stdin),
            Ok(Payload::Text("1111".into()))
        );
        assert!(
            classify("./missing.png", false, no_stdin)
                .unwrap_err()
                .contains("--text")
        );
        assert_eq!(
            classify("./missing.png", true, no_stdin),
            Ok(Payload::Text("./missing.png".into()))
        );
        let dir = tempfile::tempdir().unwrap();
        let existing = write(dir.path(), "note", b"hello");
        assert_eq!(
            classify(&existing, true, no_stdin),
            Ok(Payload::Text(existing.clone()))
        );
    }

    #[test]
    fn stdin_is_text_and_preserved() {
        assert_eq!(
            classify("-", false, || Ok(b"a\nb\n".to_vec())),
            Ok(Payload::Text("a\nb\n".into()))
        );
        assert!(classify("-", false, || Ok(vec![0xff, 0xfe])).is_err());
    }

    fn device(uin: u64, file: bool) -> Device {
        Device {
            uin,
            name: format!("d{uin}"),
            platform: 1,
            supports_file_transfer: file,
        }
    }

    #[test]
    fn peer_selection() {
        let devices = [device(1, true), device(2, true), device(3, false)];
        assert_eq!(select_peer(&devices, 1, None), Ok(2));
        assert_eq!(select_peer(&devices, 1, Some(2)), Ok(2));
        assert!(
            select_peer(&devices, 1, Some(3))
                .unwrap_err()
                .contains("support")
        );
        assert!(
            select_peer(&devices, 1, Some(1))
                .unwrap_err()
                .contains("not paired")
        );
        assert!(
            select_peer(&devices, 1, Some(9))
                .unwrap_err()
                .contains("not paired")
        );
        let many = [device(1, true), device(2, true), device(4, true)];
        assert!(select_peer(&many, 1, None).unwrap_err().contains("--peer"));
        assert!(select_peer(&[device(1, true)], 1, None).is_err());
    }
}
