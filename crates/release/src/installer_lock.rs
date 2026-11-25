//! Compatibility with released installers that arbitrate by creating `.lock`.
//! Callers hold the new kernel lock while acquiring, recovering or dropping
//! this guard. An unmarked legacy lock is never reclaimed.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

pub const MIGRATION: &[u8] = b"metro-update-lock-v2\n";
const OWNER: &str = "owner.metro-lock-v2";

pub fn migrated(root: &Path, marker: &str) -> bool {
    read_plain(&root.join(marker)).is_ok_and(|s| s.as_bytes() == MIGRATION)
}

fn read_plain(path: &Path) -> io::Result<String> {
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        opts.custom_flags(0x0020_0000);
    }
    let file = opts.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut token = String::new();
    file.take(129).read_to_string(&mut token)?;
    if token.len() > 128 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(token)
}

fn marked(path: &Path) -> Option<String> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return None;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return None;
        }
    }
    let mut entries = fs::read_dir(path).ok()?;
    if entries.next()?.ok()?.file_name() != OWNER || entries.next().is_some() {
        return None;
    }
    let token = read_plain(&path.join(OWNER)).ok()?;
    let suffix = token.strip_prefix("metro-lock-v2:")?.strip_suffix('\n')?;
    (!suffix.is_empty()
        && suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b)))
    .then_some(token)
}

fn rename_exclusive(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        Ok(renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE)?)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
        let wide = |p: &Path| p.as_os_str().encode_wide().chain([0]).collect::<Vec<_>>();
        let (from, to) = (wide(from), wide(to));
        // SAFETY: both buffers are NUL terminated; no replacement flag.
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (from, to);
        Err(io::ErrorKind::Unsupported.into())
    }
}

pub struct LegacyLock {
    path: PathBuf,
    token: String,
}

impl LegacyLock {
    pub fn owned(&self) -> bool {
        marked(&self.path).as_deref() == Some(self.token.as_str())
    }

    /// The other side of a fork owns this guard now.
    #[doc(hidden)]
    pub fn leave(self) {
        let mut this = self;
        this.token.clear();
    }

    pub fn take(root: &Path) -> io::Result<Self> {
        let path = root.join(".lock");
        match fs::symlink_metadata(&path) {
            Ok(_) if marked(&path).is_some() => retire(&path)?,
            Ok(_) => return Err(io::ErrorKind::WouldBlock.into()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        // Publish a complete guard atomically, without replacing even an
        // empty directory created concurrently by a released installer.
        let name = crate::atomic_temp_name("metro-lock");
        let tmp = root.join(&name);
        fs::create_dir(&tmp)?;
        let token = format!("metro-lock-v2:{name}\n");
        let result = crate::write_atomic(&tmp.join(OWNER), token.as_bytes())
            .and_then(|()| rename_exclusive(&tmp, &path));
        if let Err(e) = result {
            let _ = fs::remove_file(tmp.join(OWNER));
            let _ = fs::remove_dir(tmp);
            return Err(
                if e.kind() == io::ErrorKind::AlreadyExists || path.exists() {
                    io::ErrorKind::WouldBlock.into()
                } else {
                    e
                },
            );
        }
        Ok(Self { path, token })
    }
}

fn retire(path: &Path) -> io::Result<()> {
    let tomb = path.with_file_name(crate::atomic_temp_name("metro-unlock"));
    rename_exclusive(path, &tomb)?;
    // A crash here leaves an inert, uniquely named directory, never a
    // legacy lock whose owner record has been partially removed.
    let _ = fs::remove_file(tomb.join(OWNER));
    let _ = fs::remove_dir(tomb);
    Ok(())
}

impl Drop for LegacyLock {
    fn drop(&mut self) {
        if self.owned() {
            let _ = retire(&self.path);
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn reclaims_only_our_crashed_guard_and_never_releases_a_replacement() {
        let root = tempfile::tempdir().unwrap();
        let kernel = fs::File::open(root.path()).unwrap();
        kernel.try_lock().unwrap();
        let old = LegacyLock::take(root.path()).unwrap();
        let path = old.path.clone();
        let token = old.token.clone();
        old.leave();
        drop(kernel);
        let kernel = fs::File::open(root.path()).unwrap();
        kernel.try_lock().unwrap();
        let new = LegacyLock::take(root.path()).unwrap();
        assert_ne!(new.token, token);
        fs::write(path.join(OWNER), b"foreign lock").unwrap();
        drop(new);
        assert_eq!(fs::read(path.join(OWNER)).unwrap(), b"foreign lock");
        assert_eq!(
            LegacyLock::take(root.path()).err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
    }
}
