//! Workspace-confined file access.
//!
//! The workspace root is opened once as a directory file descriptor. Every
//! agent-supplied path is resolved by the kernel with
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` relative to that fd, so
//! `..` components, absolute symlinks and relative symlinks that would leave
//! the root are rejected atomically at open time (no check-then-open race).
//! Symlinks that stay inside the workspace are followed.
//!
//! Requires Linux >= 5.6. On older kernels every operation fails closed.

use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

pub const MAX_PATH_LEN: usize = 4096;
pub const MAX_READ_BYTES: usize = 1024 * 1024;
pub const MAX_LIST_ENTRIES: usize = 2000;

#[derive(Debug)]
pub enum ConfineError {
    InvalidPath(String),
    Escape,
    NotFound,
    NotAFile,
    NotADirectory,
    Unsupported(String),
    Io(io::Error),
}

impl fmt::Display for ConfineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfineError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            ConfineError::Escape => f.write_str("path escapes the workspace"),
            ConfineError::NotFound => f.write_str("no such file or directory"),
            ConfineError::NotAFile => f.write_str("not a regular file"),
            ConfineError::NotADirectory => f.write_str("not a directory"),
            ConfineError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ConfineError::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl std::error::Error for ConfineError {}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

pub(crate) const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
pub(crate) const RESOLVE_NO_SYMLINKS: u64 = 0x04;
pub(crate) const RESOLVE_BENEATH: u64 = 0x08;

#[derive(Debug)]
pub struct Workspace {
    root: OwnedFd,
    root_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirEntry {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<DirEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct FileContents {
    pub text: String,
    pub size: u64,
    pub truncated: bool,
}

impl Workspace {
    /// Open the workspace root. `path` should already be canonical.
    pub fn open(path: &Path) -> Result<Self, ConfineError> {
        let c = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_| ConfineError::InvalidPath("NUL in workspace path".into()))?;
        // SAFETY: c is a valid NUL-terminated string.
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(ConfineError::Io(io::Error::last_os_error()));
        }
        // SAFETY: fd was just returned by open and is owned here.
        let root = unsafe { OwnedFd::from_raw_fd(fd) };
        let ws = Workspace {
            root,
            root_path: path.to_path_buf(),
        };
        // Probe openat2 support now so an old kernel fails at startup.
        ws.open_beneath(".", libc::O_PATH | libc::O_DIRECTORY)?;
        Ok(ws)
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    /// Wrap an already-opened directory fd whose canonical path is
    /// `root_path`. Probes openat2 like [`Workspace::open`].
    pub(crate) fn from_fd(root: OwnedFd, root_path: PathBuf) -> Result<Self, ConfineError> {
        let ws = Workspace { root, root_path };
        ws.open_beneath(".", libc::O_PATH | libc::O_DIRECTORY)?;
        Ok(ws)
    }

    pub(crate) fn root_fd(&self) -> BorrowedFd<'_> {
        self.root.as_fd()
    }

    /// (st_dev, st_ino) of the open root directory.
    pub fn root_identity(&self) -> Result<(u64, u64), ConfineError> {
        let st = fstat(&self.root)?;
        Ok((st.st_dev, st.st_ino))
    }

    /// Check that `root_path` still names the directory held open as the
    /// root (same canonical path, same device and inode). Subprocesses and
    /// docker bind mounts are given a path, not the fd, so this is checked
    /// right before every spawn; a swapped-in symlink or directory fails.
    pub fn verify_root_path(&self) -> Result<(), ConfineError> {
        use std::os::unix::fs::MetadataExt;
        let canon = std::fs::canonicalize(&self.root_path).map_err(ConfineError::Io)?;
        if canon != self.root_path {
            return Err(ConfineError::Escape);
        }
        let md = std::fs::symlink_metadata(&self.root_path).map_err(ConfineError::Io)?;
        if !md.is_dir() || (md.dev(), md.ino()) != self.root_identity()? {
            return Err(ConfineError::Escape);
        }
        Ok(())
    }

    fn open_beneath(&self, rel: &str, flags: i32) -> Result<OwnedFd, ConfineError> {
        let rel = validate_rel(rel)?;
        openat2(
            self.root.as_fd(),
            rel,
            flags,
            RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS,
        )
    }

    /// Read a regular file (UTF-8 text), at most [`MAX_READ_BYTES`].
    pub fn read(&self, rel: &str) -> Result<FileContents, ConfineError> {
        if rel.is_empty() {
            return Err(ConfineError::InvalidPath("path is required".into()));
        }
        // O_NONBLOCK so a FIFO cannot hang the open; we reject non-regular files below.
        let fd = self.open_beneath(rel, libc::O_RDONLY | libc::O_NONBLOCK)?;
        let st = fstat(&fd)?;
        if st.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(ConfineError::NotAFile);
        }
        let size = st.st_size.max(0) as u64;
        let mut buf = Vec::with_capacity((size as usize).min(MAX_READ_BYTES));
        let file = File::from(fd);
        file.take(MAX_READ_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(ConfineError::Io)?;
        let truncated = buf.len() > MAX_READ_BYTES;
        if truncated {
            buf.truncate(MAX_READ_BYTES);
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) if truncated && e.utf8_error().error_len().is_none() => {
                // Cut inside a multi-byte char at the limit: drop the partial char.
                let valid = e.utf8_error().valid_up_to();
                let mut b = e.into_bytes();
                b.truncate(valid);
                String::from_utf8(b).expect("valid prefix")
            }
            Err(_) => {
                return Err(ConfineError::Unsupported(
                    "file is not valid UTF-8 text".into(),
                ));
            }
        };
        Ok(FileContents {
            text,
            size,
            truncated,
        })
    }

    /// List a directory (non-recursive), sorted by name, without following
    /// entry symlinks.
    pub fn list(&self, rel: &str) -> Result<Listing, ConfineError> {
        let rel_norm = if rel.is_empty() { "." } else { rel };
        let fd = self.open_beneath(rel_norm, libc::O_RDONLY | libc::O_DIRECTORY)?;
        let raw = fd.into_raw_fd();
        // SAFETY: raw is a valid directory fd; on success the DIR owns it.
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            let e = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so we still own raw.
            unsafe { libc::close(raw) };
            return Err(ConfineError::Io(e));
        }
        let dir = DirGuard(dir);
        // SAFETY: dir.0 is a valid DIR*.
        let dfd = unsafe { libc::dirfd(dir.0) };

        let mut entries = Vec::new();
        let mut truncated = false;
        loop {
            // SAFETY: dir.0 is valid; errno is reset to detect errors vs end.
            unsafe { *libc::__errno_location() = 0 };
            let ent = unsafe { libc::readdir(dir.0) };
            if ent.is_null() {
                let errno = io::Error::last_os_error();
                if errno.raw_os_error().unwrap_or(0) != 0 {
                    return Err(ConfineError::Io(errno));
                }
                break;
            }
            // SAFETY: ent points to a valid dirent until the next readdir.
            let name_c = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
            let name_b = name_c.to_bytes();
            if name_b == b"." || name_b == b".." {
                continue;
            }
            if entries.len() >= MAX_LIST_ENTRIES {
                truncated = true;
                break;
            }
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: dfd and name_c are valid; st is writable.
            let r =
                unsafe { libc::fstatat(dfd, name_c.as_ptr(), &mut st, libc::AT_SYMLINK_NOFOLLOW) };
            let (kind, size) = if r != 0 {
                ("unknown", None)
            } else {
                match st.st_mode & libc::S_IFMT {
                    libc::S_IFREG => ("file", Some(st.st_size.max(0) as u64)),
                    libc::S_IFDIR => ("dir", None),
                    libc::S_IFLNK => ("symlink", None),
                    _ => ("other", None),
                }
            };
            entries.push(DirEntry {
                name: String::from_utf8_lossy(name_b).into_owned(),
                kind,
                size,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Listing {
            path: rel_norm.to_string(),
            entries,
            truncated,
        })
    }
}

struct DirGuard(*mut libc::DIR);

impl Drop for DirGuard {
    fn drop(&mut self) {
        // SAFETY: self.0 came from a successful fdopendir and is closed once.
        unsafe { libc::closedir(self.0) };
    }
}

fn fstat(fd: &OwnedFd) -> Result<libc::stat, ConfineError> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd is valid; st is writable.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(ConfineError::Io(io::Error::last_os_error()));
    }
    Ok(st)
}

/// `openat2(dirfd, rel, flags, resolve)` with the error mapping used by this
/// module. `rel` must already be validated.
pub(crate) fn openat2(
    dirfd: BorrowedFd<'_>,
    rel: &str,
    flags: i32,
    resolve: u64,
) -> Result<OwnedFd, ConfineError> {
    let c = CString::new(rel).map_err(|_| ConfineError::InvalidPath("NUL byte".into()))?;
    // openat2 rejects O_PATH combined with unrelated flags (EINVAL).
    let extra = if flags & libc::O_PATH != 0 {
        libc::O_CLOEXEC
    } else {
        libc::O_CLOEXEC | libc::O_NOCTTY
    };
    let how = OpenHow {
        flags: (flags | extra) as u64,
        mode: 0,
        resolve,
    };
    // SAFETY: all pointers are valid for the duration of the call and the
    // struct layout matches `struct open_how` (3 x u64).
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd.as_raw_fd(),
            c.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::EXDEV) | Some(libc::ELOOP) => ConfineError::Escape,
            Some(libc::ENOENT) => ConfineError::NotFound,
            Some(libc::ENOTDIR) => ConfineError::NotADirectory,
            Some(libc::ENOSYS) => {
                ConfineError::Unsupported("openat2(2) unavailable (Linux >= 5.6 required)".into())
            }
            _ => ConfineError::Io(e),
        });
    }
    // SAFETY: fd is a fresh descriptor returned by the kernel.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Lexical validation before handing the path to the kernel. The kernel
/// (`RESOLVE_BENEATH`) is the real enforcement; this gives clearer errors and
/// rejects absolute paths outright.
fn validate_rel(rel: &str) -> Result<&str, ConfineError> {
    if rel.len() > MAX_PATH_LEN {
        return Err(ConfineError::InvalidPath("path too long".into()));
    }
    if rel.contains('\0') {
        return Err(ConfineError::InvalidPath("NUL byte".into()));
    }
    if rel.starts_with('/') {
        return Err(ConfineError::InvalidPath(
            "absolute paths are not allowed; use a path relative to the workspace".into(),
        ));
    }
    let mut depth: i64 = 0;
    for comp in rel.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return Err(ConfineError::Escape);
                }
            }
            _ => depth += 1,
        }
    }
    Ok(if rel.is_empty() { "." } else { rel })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn ws() -> (tempfile::TempDir, tempfile::TempDir, Workspace) {
        let outer = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir_in(outer.path()).unwrap();
        std::fs::write(outer.path().join("secret.txt"), "TOP SECRET").unwrap();
        std::fs::create_dir(root.path().join("sub")).unwrap();
        std::fs::write(root.path().join("sub/a.txt"), "hello").unwrap();
        std::fs::write(root.path().join("b.txt"), "bee").unwrap();
        let w = Workspace::open(&std::fs::canonicalize(root.path()).unwrap()).unwrap();
        (outer, root, w)
    }

    #[test]
    fn reads_inside() {
        let (_o, _r, w) = ws();
        assert_eq!(w.read("sub/a.txt").unwrap().text, "hello");
        assert_eq!(w.read("./sub/../b.txt").unwrap().text, "bee");
    }

    #[test]
    fn rejects_traversal_and_absolute() {
        let (o, r, w) = ws();
        assert!(matches!(w.read("../secret.txt"), Err(ConfineError::Escape)));
        assert!(matches!(
            w.read("sub/../../secret.txt"),
            Err(ConfineError::Escape)
        ));
        assert!(matches!(
            w.read("/etc/passwd"),
            Err(ConfineError::InvalidPath(_))
        ));
        let abs = o.path().join("secret.txt");
        assert!(w.read(abs.to_str().unwrap()).is_err());
        assert!(matches!(w.list(".."), Err(ConfineError::Escape)));
        assert!(w.read("").is_err());
        assert!(w.read("a\0b").is_err());
        drop(r);
    }

    #[test]
    fn rejects_symlink_escapes() {
        let (o, r, w) = ws();
        symlink(o.path().join("secret.txt"), r.path().join("abs_link")).unwrap();
        symlink("../secret.txt", r.path().join("rel_link")).unwrap();
        symlink(o.path(), r.path().join("dir_link")).unwrap();
        symlink("/proc/self/root/etc/passwd", r.path().join("proc_link")).unwrap();
        assert!(matches!(w.read("abs_link"), Err(ConfineError::Escape)));
        assert!(matches!(w.read("rel_link"), Err(ConfineError::Escape)));
        assert!(matches!(
            w.read("dir_link/secret.txt"),
            Err(ConfineError::Escape)
        ));
        assert!(matches!(w.list("dir_link"), Err(ConfineError::Escape)));
        assert!(w.read("proc_link").is_err());
        // Listing shows the links without following them.
        let l = w.list(".").unwrap();
        let k: Vec<_> = l
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind))
            .collect();
        assert!(k.contains(&("abs_link", "symlink")));
        assert!(k.contains(&("sub", "dir")));
    }

    #[test]
    fn follows_internal_symlinks() {
        let (_o, r, w) = ws();
        symlink("sub/a.txt", r.path().join("inner")).unwrap();
        symlink("../b.txt", r.path().join("sub/up")).unwrap();
        assert_eq!(w.read("inner").unwrap().text, "hello");
        assert_eq!(w.read("sub/up").unwrap().text, "bee");
    }

    #[test]
    fn rejects_non_regular_and_binary() {
        let (_o, r, w) = ws();
        assert!(matches!(w.read("sub"), Err(ConfineError::NotAFile)));
        let fifo = CString::new(r.path().join("fifo").to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(matches!(w.read("fifo"), Err(ConfineError::NotAFile)));
        std::fs::write(r.path().join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        assert!(matches!(w.read("bin"), Err(ConfineError::Unsupported(_))));
        assert!(matches!(w.list("b.txt"), Err(ConfineError::NotADirectory)));
        assert!(matches!(w.read("nope"), Err(ConfineError::NotFound)));
    }

    #[test]
    fn read_is_bounded() {
        let (_o, r, w) = ws();
        std::fs::write(r.path().join("big"), "x".repeat(MAX_READ_BYTES + 10)).unwrap();
        let c = w.read("big").unwrap();
        assert!(c.truncated);
        assert_eq!(c.text.len(), MAX_READ_BYTES);
        assert_eq!(c.size, (MAX_READ_BYTES + 10) as u64);
    }

    #[test]
    fn list_sorted() {
        let (_o, _r, w) = ws();
        let l = w.list("").unwrap();
        let names: Vec<_> = l.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["b.txt", "sub"]);
        assert_eq!(l.entries[0].size, Some(3));
    }
}
