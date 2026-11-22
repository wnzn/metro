//! Private directories (mode 700) and files (mode 600), written atomically
//! and never through a symlink.
//!
//! On Windows the files rely on the per-user profile ACLs of the config
//! directory; there is no extra protection (documented in docs/connector.md).

use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::Secret;

/// Create `dir` (and parents) if needed and make sure it is a real directory
/// (not a symlink) that only the current user can open.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(dir) {
        if meta.file_type().is_symlink() {
            bail!("{} is a symlink; refusing to use it", dir.display());
        }
        if !meta.is_dir() {
            bail!("{} exists and is not a directory", dir.display());
        }
    } else {
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .with_context(|| format!("creating {}", dir.display()))?;
        }
        #[cfg(not(unix))]
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::metadata(dir)?;
        // SAFETY: geteuid has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if meta.uid() != uid {
            bail!(
                "{} belongs to another user; refusing to use it",
                dir.display()
            );
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("setting mode 700 on {}", dir.display()))?;
        }
    }
    Ok(())
}

/// Atomically write `data` to `path` with mode 600: write a new temp file in
/// the same directory (O_EXCL, O_NOFOLLOW), fsync, rename over the target.
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("file has no parent directory")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("bad file name")?;
    let tmp = dir.join(format!(".{name}.tmp-{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| -> Result<()> {
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
        if let Ok(meta) = fs::symlink_metadata(path)
            && meta.is_dir()
        {
            bail!("{} is a directory", path.display());
        }
        fs::rename(&tmp, path).with_context(|| format!("saving {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Read a private file of at most `cap` bytes. `Ok(None)` if it doesn't
/// exist. Refuses symlinks, and on Unix files readable by group or others.
pub fn read_private(path: &Path, cap: u64) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    if !meta.is_file() {
        bail!("{} is not a regular file", path.display());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "{} is readable by other users; run `chmod 600 {}`",
                path.display(),
                path.display()
            );
        }
    }
    if meta.len() > cap {
        bail!("{} is too large", path.display());
    }
    let mut buf = Vec::new();
    fs::File::open(path)?.take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        bail!("{} is too large", path.display());
    }
    Ok(Some(buf))
}

/// Remove a file if it exists, after overwriting its contents with zeros
/// (best effort; filesystems may keep old blocks).
pub fn remove_private(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => {
            if let Ok(mut f) = fs::OpenOptions::new().write(true).open(path) {
                let zeros = vec![0u8; m.len().min(1 << 20) as usize];
                let _ = f.write_all(&zeros);
                let _ = f.sync_all();
            }
            fs::remove_file(path)?;
            Ok(true)
        }
        Ok(_) => {
            fs::remove_file(path)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Read a secret from a file given by the person (`-` = stdin). Takes the
/// first line, trimmed. The file may be anywhere, but on Unix it must not be
/// readable by other users.
pub fn read_secret_input(source: &str, what: &str) -> Result<Secret> {
    let raw = if source == "-" {
        let mut s = String::new();
        std::io::stdin()
            .take(64 * 1024)
            .read_to_string(&mut s)
            .with_context(|| format!("reading the {what} from stdin"))?;
        s
    } else {
        let path = Path::new(source);
        let bytes = read_private(path, 64 * 1024)?
            .with_context(|| format!("{} does not exist", path.display()))?;
        String::from_utf8(bytes).with_context(|| format!("the {what} file is not UTF-8"))?
    };
    let line = raw.lines().next().unwrap_or("").trim().to_string();
    let mut raw = raw;
    zeroize::Zeroize::zeroize(&mut raw);
    Ok(Secret::new(line))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn private_files_and_dirs() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("a/b");
        ensure_private_dir(&d).unwrap();
        assert_eq!(
            fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let f = d.join("x");
        write_private(&f, b"hello").unwrap();
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(read_private(&f, 10).unwrap().unwrap(), b"hello");
        assert!(read_private(&f, 2).is_err());
        write_private(&f, b"again").unwrap();
        assert_eq!(read_private(&f, 10).unwrap().unwrap(), b"again");

        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private(&f, 10).is_err());

        let link = t.path().join("link");
        std::os::unix::fs::symlink(&d, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        let flink = d.join("flink");
        std::os::unix::fs::symlink(&f, &flink).unwrap();
        assert!(read_private(&flink, 10).is_err());

        assert!(remove_private(&f).unwrap());
        assert!(!remove_private(&f).unwrap());
        assert!(read_private(&f, 10).unwrap().is_none());
    }
}
