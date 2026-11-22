//! Per-Space workspaces for reusable (`metro_rt_v2_`) runner credentials.
//!
//! `METRO_WORKSPACE` is the runner's writable base. Each Space gets
//! `<base>/spaces/<space_id>/`, created on first use. The id comes from the
//! server's claim and must pass [`valid_space_id`]; it is never taken from
//! tool arguments. Both components are created with `mkdirat` and opened
//! with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` relative to the
//! base fd, so a symlinked `spaces/` or `spaces/<id>` (for example one Space
//! aliased to another) is refused rather than followed. Inside a Space root
//! the usual [`Workspace`] rules apply: paths stay beneath that root, so
//! `../<other-space>` and symlinks into a sibling Space are rejected.
//!
//! No tool is ever bound to the base itself.

use std::io;
use std::path::{Path, PathBuf};

use crate::client::valid_space_id;
use crate::confine::{
    self, ConfineError, RESOLVE_BENEATH, RESOLVE_NO_MAGICLINKS, RESOLVE_NO_SYMLINKS, Workspace,
};

pub const SPACES_DIR: &str = "spaces";
const SPACES_DIR_MODE: libc::mode_t = 0o711;
/// World-readable (subject to umask) so a non-root docker sandbox user can
/// read the read-only bind mount. Only the runner writes here.
const SPACE_DIR_MODE: libc::mode_t = 0o755;
const STRICT: u64 = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS;

#[derive(Debug)]
pub struct SpaceRoots {
    base: Workspace,
}

fn mkdirat(
    dir: std::os::fd::BorrowedFd<'_>,
    name: &str,
    mode: libc::mode_t,
) -> Result<(), ConfineError> {
    use std::os::fd::AsRawFd;
    let c =
        std::ffi::CString::new(name).map_err(|_| ConfineError::InvalidPath("NUL byte".into()))?;
    // SAFETY: valid dirfd and NUL-terminated name.
    if unsafe { libc::mkdirat(dir.as_raw_fd(), c.as_ptr(), mode) } == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if e.raw_os_error() == Some(libc::EEXIST) {
        // Whatever exists is checked by the strict open that follows.
        Ok(())
    } else {
        Err(ConfineError::Io(e))
    }
}

impl SpaceRoots {
    /// Open the base and make sure `spaces/` exists and is a real directory.
    /// `base` should already be canonical.
    pub fn open(base: &Path) -> Result<Self, ConfineError> {
        let roots = SpaceRoots {
            base: Workspace::open(base)?,
        };
        roots.spaces_dir()?;
        Ok(roots)
    }

    pub fn base_path(&self) -> &Path {
        self.base.root_path()
    }

    fn spaces_dir(&self) -> Result<std::os::fd::OwnedFd, ConfineError> {
        mkdirat(self.base.root_fd(), SPACES_DIR, SPACES_DIR_MODE)?;
        confine::openat2(
            self.base.root_fd(),
            SPACES_DIR,
            libc::O_PATH | libc::O_DIRECTORY,
            STRICT,
        )
    }

    /// Path a Space root lives at (whether or not it exists yet).
    pub fn space_path(&self, space_id: &str) -> Result<PathBuf, ConfineError> {
        if !valid_space_id(space_id) {
            return Err(ConfineError::InvalidPath(format!(
                "invalid Space id {space_id:?}"
            )));
        }
        Ok(self.base.root_path().join(SPACES_DIR).join(space_id))
    }

    /// Open (creating if needed) the root for one Space. Fresh on every call,
    /// so nothing opened for a previous job or Space is reused.
    pub fn open_space(&self, space_id: &str) -> Result<Workspace, ConfineError> {
        let path = self.space_path(space_id)?;
        let spaces = self.spaces_dir()?;
        use std::os::fd::AsFd;
        mkdirat(spaces.as_fd(), space_id, SPACE_DIR_MODE)?;
        let fd = confine::openat2(
            spaces.as_fd(),
            space_id,
            libc::O_PATH | libc::O_DIRECTORY,
            STRICT,
        )?;
        let ws = Workspace::from_fd(fd, path)?;
        ws.verify_root_path()?;
        Ok(ws)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn base() -> (tempfile::TempDir, SpaceRoots) {
        let d = tempfile::tempdir().unwrap();
        let p = std::fs::canonicalize(d.path()).unwrap();
        let r = SpaceRoots::open(&p).unwrap();
        (d, r)
    }

    #[test]
    fn creates_and_isolates_spaces() {
        let (d, r) = base();
        std::fs::write(d.path().join("root-secret"), "x").unwrap();
        let a = r.open_space("s-a").unwrap();
        let b = r.open_space("s-b").unwrap();
        std::fs::write(d.path().join("spaces/s-a/ma"), "A").unwrap();
        std::fs::write(d.path().join("spaces/s-b/mb"), "B").unwrap();
        assert_eq!(a.read("ma").unwrap().text, "A");
        assert!(matches!(a.read("../s-b/mb"), Err(ConfineError::Escape)));
        assert!(matches!(
            a.read("../../root-secret"),
            Err(ConfineError::Escape)
        ));
        let names: Vec<_> = b
            .list(".")
            .unwrap()
            .entries
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["mb"]);
        assert!(a.root_path().ends_with("spaces/s-a"));
    }

    #[test]
    fn rejects_bad_ids() {
        let (_d, r) = base();
        for bad in ["", "..", ".", "../x", "a/b", "S-A", "-x"] {
            assert!(r.open_space(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn refuses_symlinked_space_or_spaces_dir() {
        let (d, r) = base();
        r.open_space("s-b").unwrap();
        std::fs::write(d.path().join("spaces/s-b/mb"), "B").unwrap();
        // One Space aliased to another.
        symlink("s-b", d.path().join("spaces/s-c")).unwrap();
        assert!(matches!(r.open_space("s-c"), Err(ConfineError::Escape)));
        // A symlink to outside.
        let other = tempfile::tempdir().unwrap();
        symlink(other.path(), d.path().join("spaces/s-d")).unwrap();
        assert!(matches!(r.open_space("s-d"), Err(ConfineError::Escape)));

        // Symlinked `spaces/` itself.
        let d2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(d2.path().join("real")).unwrap();
        symlink("real", d2.path().join("spaces")).unwrap();
        assert!(SpaceRoots::open(&std::fs::canonicalize(d2.path()).unwrap()).is_err());
    }

    #[test]
    fn detects_root_path_swap() {
        let (d, r) = base();
        let a = r.open_space("s-a").unwrap();
        a.verify_root_path().unwrap();
        std::fs::rename(d.path().join("spaces/s-a"), d.path().join("spaces/moved")).unwrap();
        std::fs::create_dir(d.path().join("spaces/s-a")).unwrap();
        assert!(a.verify_root_path().is_err());
    }
}
