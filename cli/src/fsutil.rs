//! Safe placement of received data into the user's output directory.
use std::{
    fs::{self, OpenOptions},
    io,
    path::{Path, PathBuf},
};

/// `name`, `name (1)`, `name (2)` ... keeping the extension.
fn candidate(name: &str, index: u32) -> String {
    if index == 0 {
        return name.to_owned();
    }
    match name.rfind('.').filter(|dot| *dot > 0) {
        Some(dot) => format!("{} ({index}){}", &name[..dot], &name[dot..]),
        None => format!("{name} ({index})"),
    }
}

/// Reduces an untrusted name to a single path component.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    match base.trim() {
        "" | "." | ".." => "file".to_owned(),
        value => value.to_owned(),
    }
}

/// Moves `source` into `dir` without ever replacing an existing file; returns the final path.
/// Falls back to copy+remove when hard links are unavailable (different filesystem, FAT).
pub fn commit_file(source: &Path, dir: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let name = safe_name(name);
    for index in 0..10_000 {
        let destination = dir.join(candidate(&name, index));
        match fs::hard_link(source, &destination) {
            Ok(()) => {
                fs::remove_file(source)?;
                return Ok(destination);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
            {
                Ok(mut out) => {
                    let result = fs::File::open(source)
                        .and_then(|mut input| io::copy(&mut input, &mut out))
                        .and_then(|_| out.sync_all());
                    if let Err(e) = result {
                        drop(out);
                        let _ = fs::remove_file(&destination);
                        return Err(e);
                    }
                    fs::remove_file(source)?;
                    return Ok(destination);
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            },
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with the same name",
    ))
}

/// Writes `bytes` to a new file in `dir` under a unique name.
pub fn write_unique(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    use io::Write;
    fs::create_dir_all(dir)?;
    let name = safe_name(name);
    for index in 0..10_000 {
        let destination = dir.join(candidate(&name, index));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
                return Ok(destination);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many files with the same name",
    ))
}

/// Creates a private (0700 on Unix) directory.
pub fn private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Moves an existing state directory aside to `<dir>.old-<unix seconds>`; `None` if absent.
pub fn backup_dir(dir: &Path) -> io::Result<Option<PathBuf>> {
    if !dir.exists() {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".old-{stamp}"));
    let backup = dir.with_file_name(name);
    if backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "backup directory already exists",
        ));
    }
    fs::rename(dir, &backup)?;
    Ok(Some(backup))
}

/// Undoes `backup_dir`: discards whatever replaced `dir` and moves the backup back.
pub fn restore_dir(dir: &Path, backup: &Path) -> io::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
    }
    fs::rename(backup, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_names_keep_extension() {
        assert_eq!(candidate("a.txt", 0), "a.txt");
        assert_eq!(candidate("a.txt", 2), "a (2).txt");
        assert_eq!(candidate("archive.tar.gz", 1), "archive.tar (1).gz");
        assert_eq!(candidate("noext", 1), "noext (1)");
        assert_eq!(candidate(".hidden", 1), ".hidden (1)");
    }

    #[test]
    fn names_are_reduced_to_one_component() {
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name("C:\\x\\y.txt"), "y.txt");
        assert_eq!(safe_name(".."), "file");
        assert_eq!(safe_name(""), "file");
        assert_eq!(safe_name("ok.txt"), "ok.txt");
    }

    #[test]
    fn commit_never_overwrites() {
        let staging = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        fs::write(out.path().join("a.txt"), "existing").unwrap();
        let src = staging.path().join("tmp");
        fs::write(&src, "new").unwrap();
        let first = commit_file(&src, out.path(), "a.txt").unwrap();
        assert_eq!(first, out.path().join("a (1).txt"));
        assert_eq!(
            fs::read_to_string(out.path().join("a.txt")).unwrap(),
            "existing"
        );
        assert_eq!(fs::read_to_string(&first).unwrap(), "new");
        assert!(!src.exists());
        let src = staging.path().join("tmp2");
        fs::write(&src, "newer").unwrap();
        assert_eq!(
            commit_file(&src, out.path(), "a.txt").unwrap(),
            out.path().join("a (2).txt")
        );
    }

    #[test]
    fn commit_creates_missing_output_dir() {
        let staging = tempfile::tempdir().unwrap();
        let out = staging.path().join("nested/dir");
        let src = staging.path().join("tmp");
        fs::write(&src, "x").unwrap();
        let path = commit_file(&src, &out, "../evil").unwrap();
        assert_eq!(path, out.join("evil"));
    }

    #[test]
    fn write_unique_adds_suffix() {
        let out = tempfile::tempdir().unwrap();
        let a = write_unique(out.path(), "p.png", b"1").unwrap();
        let b = write_unique(out.path(), "p.png", b"2").unwrap();
        assert_eq!(a.file_name().unwrap(), "p.png");
        assert_eq!(b.file_name().unwrap(), "p (1).png");
    }

    #[test]
    fn backup_and_restore_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("wxc");
        assert_eq!(backup_dir(&state).unwrap(), None);
        fs::create_dir(&state).unwrap();
        fs::write(state.join("identity"), "old").unwrap();
        let backup = backup_dir(&state).unwrap().unwrap();
        assert!(!state.exists());
        assert_eq!(fs::read_to_string(backup.join("identity")).unwrap(), "old");
        fs::create_dir(&state).unwrap();
        fs::write(state.join("identity"), "new").unwrap();
        restore_dir(&state, &backup).unwrap();
        assert_eq!(fs::read_to_string(state.join("identity")).unwrap(), "old");
        assert!(!backup.exists());
    }
}
