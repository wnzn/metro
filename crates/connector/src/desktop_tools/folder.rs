//! Handle-relative access to one Space's local folder: list, read, and
//! create or replace one file.
//!
//! Every path an agent sends is split into plain names and opened one name
//! at a time, relative to the handle of the directory before it, with
//! symlinks never followed (`cap_primitives::fs::open_dir_nofollow` and
//! `open` with `FollowSymlinks::No`: `O_NOFOLLOW` under `openat2` /
//! `RESOLVE_BENEATH` on Linux, `FILE_FLAG_OPEN_REPARSE_POINT` on Windows).
//! On top of that every opened handle is checked: on Windows any handle with
//! `FILE_ATTRIBUTE_REPARSE_POINT` (symlink, junction, mount point, cloud
//! placeholder) is refused. So there is no check-then-open window, `..` and
//! absolute paths never reach the OS, and nothing outside the folder (a
//! sibling Space, the pairing file, the rest of the disk) can be named.
//!
//! Writing ([`Folder::write`]) creates missing parent directories (0755)
//! one name at a time under the same rules, writes a new file beside the target and
//! renames it over the target. A link is never written through or replaced,
//! and a hard-linked target is replaced, not modified in place. Nothing here
//! deletes or renames the person's files.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use cap_primitives::fs::{DirOptions, FollowSymlinks, OpenOptions};

pub const MAX_PATH_LEN: usize = 4096;
pub const MAX_DEPTH: usize = 64;
pub const MAX_NAME_LEN: usize = 255;
/// Leaves room for the truncation note under Metro's 1 MiB result limit.
pub const MAX_READ_BYTES: usize = 1024 * 1024 - 4096;
pub const MAX_LIST_ENTRIES: usize = 2000;
pub const MAX_WRITE_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub enum FolderError {
    InvalidPath(String),
    NotFound,
    NotAFile,
    NotADirectory,
    /// A symlink, junction or other reparse point: never followed.
    Link,
    Unsupported(String),
    Io(io::Error),
}

impl fmt::Display for FolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FolderError::InvalidPath(m) => write!(f, "invalid path: {m}"),
            FolderError::NotFound => f.write_str("no such file or directory"),
            FolderError::NotAFile => f.write_str("not a regular file"),
            FolderError::NotADirectory => f.write_str("not a directory"),
            FolderError::Link => {
                f.write_str("symlinks, junctions and other links are not followed")
            }
            FolderError::Unsupported(m) => write!(f, "unsupported: {m}"),
            // Kind only: an OS message can carry a host path.
            FolderError::Io(e) => write!(f, "I/O error ({:?})", e.kind()),
        }
    }
}

impl std::error::Error for FolderError {}

fn map_open_err(e: io::Error) -> FolderError {
    if e.kind() == io::ErrorKind::NotFound {
        return FolderError::NotFound;
    }
    #[cfg(unix)]
    if let Some(code) = e.raw_os_error() {
        // O_NOFOLLOW on a symlink: ELOOP; on Linux with O_DIRECTORY on a
        // symlink also ENOTDIR (then checked below by the caller's kind).
        if code == libc::ELOOP || code == libc::EMLINK {
            return FolderError::Link;
        }
        if code == libc::ENOTDIR {
            return FolderError::NotADirectory;
        }
        if code == libc::EXDEV {
            return FolderError::Link;
        }
    }
    #[cfg(windows)]
    if let Some(code) = e.raw_os_error() {
        // ERROR_STOPPED_ON_SYMLINK (681), ERROR_CANT_ACCESS_FILE (1920),
        // ERROR_CANT_RESOLVE_FILENAME (1921), ERROR_DIRECTORY (267).
        match code {
            681 | 1920 | 1921 => return FolderError::Link,
            267 => return FolderError::NotADirectory,
            _ => {}
        }
    }
    match e.kind() {
        io::ErrorKind::NotADirectory => FolderError::NotADirectory,
        _ => FolderError::Io(e),
    }
}

/// Names Windows treats as devices (with or without an extension), refused
/// on every platform so a folder behaves the same everywhere.
const DEVICE_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "clock$", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "com¹", "com²", "com³", "lpt1", "lpt2", "lpt3", "lpt4",
    "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "lpt¹", "lpt²", "lpt³",
];

/// One plain file or directory name, the same rules on Linux and Windows:
/// no separators, no `.`/`..`, no `:` (alternate data streams, drives), no
/// characters Windows forbids, no trailing dot or space, no device names.
pub fn check_name(name: &str) -> Result<(), FolderError> {
    let bad = |m: &str| Err(FolderError::InvalidPath(m.to_string()));
    if name.is_empty() || name == "." || name == ".." {
        return bad("empty, '.' or '..' path component");
    }
    if name.len() > MAX_NAME_LEN {
        return bad("path component too long");
    }
    if name.chars().any(|c| {
        c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
    }) {
        return bad("path component contains a character that is not allowed");
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return bad("path component ends with a dot or a space");
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_lowercase();
    if DEVICE_NAMES.contains(&stem.as_str()) {
        return bad("path component is a reserved device name");
    }
    Ok(())
}

/// Split an agent path into checked names. `""` and `"."` are the folder.
/// Only `/` separates; `..`, absolute paths, drive letters and UNC prefixes
/// are refused outright (never normalized).
pub fn split_path(rel: &str) -> Result<Vec<&str>, FolderError> {
    if rel.len() > MAX_PATH_LEN {
        return Err(FolderError::InvalidPath("path too long".into()));
    }
    if rel.starts_with('/') || rel.starts_with('\\') {
        return Err(FolderError::InvalidPath(
            "absolute paths are not allowed; use a path relative to the folder".into(),
        ));
    }
    let mut out = Vec::new();
    for comp in rel.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                return Err(FolderError::InvalidPath("'..' is not allowed".into()));
            }
            name => {
                check_name(name)?;
                out.push(name);
            }
        }
    }
    if out.len() > MAX_DEPTH {
        return Err(FolderError::InvalidPath("path is too deep".into()));
    }
    Ok(out)
}

/// What kind of object an opened handle is, refusing reparse points.
fn handle_kind(f: &File) -> Result<std::fs::FileType, FolderError> {
    let meta = f.metadata().map_err(FolderError::Io)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(FolderError::Link);
        }
    }
    let ft = meta.file_type();
    if ft.is_symlink() {
        return Err(FolderError::Link);
    }
    Ok(ft)
}

/// Open the directory `name` inside `dir`, never following a link.
pub fn open_subdir(dir: &File, name: &str) -> Result<File, FolderError> {
    check_name(name)?;
    let f = cap_primitives::fs::open_dir_nofollow(dir, Path::new(name)).map_err(map_open_err)?;
    if !handle_kind(&f)?.is_dir() {
        return Err(FolderError::NotADirectory);
    }
    Ok(f)
}

/// Open the file `name` inside `dir` for reading, never following a link.
/// Unix: `O_NONBLOCK` so a FIFO can't hang the open; non-regular files are
/// refused right after.
pub fn open_file(dir: &File, name: &str) -> Result<File, FolderError> {
    check_name(name)?;
    let mut opts = OpenOptions::new();
    opts.read(true);
    opts._cap_fs_ext_follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_primitives::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let f = match cap_primitives::fs::open(dir, Path::new(name), &opts) {
        Ok(f) => f,
        // Windows refuses to open a directory as a file with "access denied".
        #[cfg(windows)]
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied && open_subdir(dir, name).is_ok() => {
            return Err(FolderError::NotAFile);
        }
        Err(e) => return Err(map_open_err(e)),
    };
    let ft = handle_kind(&f)?;
    if ft.is_dir() {
        return Err(FolderError::NotAFile);
    }
    if !ft.is_file() {
        return Err(FolderError::NotAFile);
    }
    Ok(f)
}

/// [`open_subdir`], creating `name` first when it is missing.
fn open_or_create_subdir(dir: &File, name: &str) -> Result<File, FolderError> {
    match open_subdir(dir, name) {
        Err(FolderError::NotFound) => {
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut o = DirOptions::new();
            #[cfg(unix)]
            {
                use cap_primitives::fs::DirBuilderExt;
                o.mode(0o755);
            }
            match cap_primitives::fs::create_dir(dir, Path::new(name), &o) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(map_open_err(e)),
            }
            open_subdir(dir, name)
        }
        r => r,
    }
}

/// Open the absolute directory `path` (a folder the person chose), refusing
/// it when its last component is a link or reparse point. Nothing is
/// created.
pub fn open_dir_path(path: &Path) -> Result<File, FolderError> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let f = opts.open(path).map_err(map_open_err)?;
    if !handle_kind(&f)?.is_dir() {
        return Err(FolderError::NotADirectory);
    }
    Ok(f)
}

/// What [`Folder::write`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub path: String,
    pub bytes: usize,
    pub created: bool,
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

/// One Space's folder, as an open directory handle.
#[derive(Debug)]
pub struct Folder {
    root: File,
}

impl Folder {
    /// Wrap a directory handle that was itself opened without following
    /// links (see [`open_subdir`]).
    pub fn from_handle(root: File) -> Self {
        Folder { root }
    }

    /// The folder's own handle (the shell's working directory on Unix).
    pub fn handle(&self) -> &File {
        &self.root
    }

    fn walk(&self, names: &[&str]) -> Result<File, FolderError> {
        let mut cur = self.root.try_clone().map_err(FolderError::Io)?;
        for name in names {
            cur = open_subdir(&cur, name)?;
        }
        Ok(cur)
    }

    /// List one directory (not recursive), sorted by name. Entries are not
    /// followed: links show as `symlink`.
    pub fn list(&self, rel: &str) -> Result<Listing, FolderError> {
        let names = split_path(rel)?;
        let dir = self.walk(&names)?;
        let dir = cap_std::fs::Dir::from_std_file(dir);
        let mut entries = Vec::new();
        let mut truncated = false;
        for ent in dir.entries().map_err(FolderError::Io)? {
            let ent = ent.map_err(FolderError::Io)?;
            if entries.len() >= MAX_LIST_ENTRIES {
                truncated = true;
                break;
            }
            let name = ent.file_name().to_string_lossy().into_owned();
            let (kind, size) = match ent.file_type() {
                Ok(t) if t.is_symlink() => ("symlink", None),
                Ok(t) if t.is_dir() => ("dir", None),
                Ok(t) if t.is_file() => ("file", ent.metadata().ok().map(|m| m.len())),
                Ok(_) => ("other", None),
                Err(_) => ("unknown", None),
            };
            entries.push(DirEntry { name, kind, size });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Listing {
            path: if names.is_empty() {
                ".".into()
            } else {
                names.join("/")
            },
            entries,
            truncated,
        })
    }

    /// Read a regular UTF-8 file, at most [`MAX_READ_BYTES`].
    pub fn read(&self, rel: &str) -> Result<FileContents, FolderError> {
        let mut names = split_path(rel)?;
        let Some(last) = names.pop() else {
            return Err(FolderError::InvalidPath("path is required".into()));
        };
        let dir = self.walk(&names)?;
        let file = open_file(&dir, last)?;
        let size = file.metadata().map_err(FolderError::Io)?.len();
        let mut buf = Vec::with_capacity((size as usize).min(MAX_READ_BYTES));
        file.take(MAX_READ_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .map_err(FolderError::Io)?;
        let truncated = buf.len() > MAX_READ_BYTES;
        if truncated {
            buf.truncate(MAX_READ_BYTES);
        }
        let text = match String::from_utf8(buf) {
            Ok(s) => s,
            Err(e) if truncated && e.utf8_error().error_len().is_none() => {
                let valid = e.utf8_error().valid_up_to();
                let mut b = e.into_bytes();
                b.truncate(valid);
                String::from_utf8(b).expect("valid prefix")
            }
            Err(_) => {
                return Err(FolderError::Unsupported(
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

    /// Create or replace the file `rel` with `content` (at most
    /// [`MAX_WRITE_BYTES`]), creating missing parent directories. Only a
    /// regular file is replaced; a link, directory or other object at `rel`
    /// is refused. On Unix a replaced file keeps its permission bits.
    pub fn write(&self, rel: &str, content: &[u8]) -> Result<Written, FolderError> {
        if content.len() > MAX_WRITE_BYTES {
            return Err(FolderError::Unsupported(format!(
                "content is larger than {MAX_WRITE_BYTES} bytes"
            )));
        }
        let mut names = split_path(rel)?;
        let Some(last) = names.pop() else {
            return Err(FolderError::InvalidPath("path is required".into()));
        };
        let mut dir = self.root.try_clone().map_err(FolderError::Io)?;
        for name in &names {
            dir = open_or_create_subdir(&dir, name)?;
        }
        let existing = match open_file(&dir, last) {
            Ok(f) => Some(f.metadata().map_err(FolderError::Io)?.permissions()),
            Err(FolderError::NotFound) => None,
            Err(e) => return Err(e),
        };
        let tmp = format!(".metro-write-{:016x}.tmp", rand::random::<u64>());
        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true);
        opts._cap_fs_ext_follow(FollowSymlinks::No);
        let written = (|| -> io::Result<()> {
            let mut f = cap_primitives::fs::open(&dir, Path::new(&tmp), &opts)?;
            f.write_all(content)?;
            #[cfg(unix)]
            if let Some(p) = &existing {
                f.set_permissions(p.clone())?;
            }
            f.sync_all()
        })()
        .and_then(|()| cap_primitives::fs::rename(&dir, Path::new(&tmp), &dir, Path::new(last)));
        if let Err(e) = written {
            let _ = cap_primitives::fs::remove_file(&dir, Path::new(&tmp));
            return Err(map_open_err(e));
        }
        names.push(last);
        Ok(Written {
            path: names.join("/"),
            bytes: content.len(),
            created: existing.is_none(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(dir: &Path) -> Folder {
        let parent = open_dir_path(dir.parent().unwrap()).unwrap();
        let name = dir.file_name().unwrap().to_str().unwrap();
        Folder::from_handle(open_subdir(&parent, name).unwrap())
    }

    #[test]
    fn names() {
        for ok in [
            "a",
            "a.txt",
            ".hidden",
            "notes.md",
            "con1",
            "lpt10.txt",
            "a b",
        ] {
            assert!(check_name(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "a:b",
            "file.txt:stream",
            "CON",
            "con.txt",
            "Nul",
            "LPT1.log",
            "COM9",
            "conin$",
            "a.",
            "a ",
            "a?",
            "a*",
            "a\0",
            "a\n",
            "<x>",
            &"x".repeat(256),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn paths() {
        assert_eq!(split_path("").unwrap(), Vec::<&str>::new());
        assert_eq!(split_path(".").unwrap(), Vec::<&str>::new());
        assert_eq!(split_path("a//b/./c").unwrap(), vec!["a", "b", "c"]);
        for bad in [
            "/etc/passwd",
            "\\\\server\\share",
            "C:/Windows",
            "C:",
            "a/../b",
            "..",
            "../sibling",
            "a\\..\\b",
        ] {
            assert!(split_path(bad).is_err(), "{bad}");
        }
        let deep = vec!["d"; MAX_DEPTH + 1].join("/");
        assert!(split_path(&deep).is_err());
    }

    #[test]
    fn reads_and_lists_inside() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::write(ws.join("hello.txt"), "hi").unwrap();
        std::fs::write(ws.join("sub/x.md"), "x").unwrap();
        let f = folder(&ws);
        assert_eq!(f.read("hello.txt").unwrap().text, "hi");
        assert_eq!(f.read("./sub/x.md").unwrap().text, "x");
        let l = f.list("").unwrap();
        let names: Vec<_> = l
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.kind))
            .collect();
        assert_eq!(names, vec![("hello.txt", "file"), ("sub", "dir")]);
        assert_eq!(f.list("sub").unwrap().path, "sub");
        assert!(matches!(f.read(""), Err(FolderError::InvalidPath(_))));
        assert!(matches!(f.read("sub"), Err(FolderError::NotAFile)));
        assert!(matches!(
            f.list("hello.txt"),
            Err(FolderError::NotADirectory)
        ));
        assert!(matches!(f.read("missing"), Err(FolderError::NotFound)));
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_links() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        let sibling = t.path().join("sibling");
        std::fs::create_dir_all(ws.join("real")).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("secret.txt"), "sibling secret").unwrap();
        std::fs::write(ws.join("real/in.txt"), "inside").unwrap();
        // Links out (absolute and relative), and even links that stay inside.
        symlink(sibling.join("secret.txt"), ws.join("abs.txt")).unwrap();
        symlink("../sibling", ws.join("rel_dir")).unwrap();
        symlink("real/in.txt", ws.join("inner.txt")).unwrap();
        symlink("real", ws.join("inner_dir")).unwrap();
        let f = folder(&ws);
        for p in [
            "abs.txt",
            "rel_dir/secret.txt",
            "inner.txt",
            "inner_dir/in.txt",
        ] {
            let e = f.read(p).unwrap_err();
            assert!(
                matches!(e, FolderError::Link | FolderError::NotADirectory),
                "{p}: {e:?}"
            );
        }
        assert!(f.list("rel_dir").is_err());
        assert!(f.list("inner_dir").is_err());
        let l = f.list(".").unwrap();
        assert!(
            l.entries
                .iter()
                .filter(|e| e.name != "real")
                .all(|e| e.kind == "symlink")
        );
        // A root folder that is itself a symlink is refused when opened.
        symlink(&sibling, t.path().join("ws_link")).unwrap();
        let parent = File::open(t.path()).unwrap();
        assert!(open_subdir(&parent, "ws_link").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_fifo_and_binary() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let fifo = std::ffi::CString::new(ws.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        std::fs::write(ws.join("bin"), [0xff, 0xfe, 0x00]).unwrap();
        let f = folder(&ws);
        assert!(matches!(f.read("pipe"), Err(FolderError::NotAFile)));
        assert!(matches!(f.read("bin"), Err(FolderError::Unsupported(_))));
    }

    #[test]
    fn writes_create_and_replace_inside() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let f = folder(&ws);
        let w = f.write("a/b/./c.txt", b"one").unwrap();
        assert_eq!(
            w,
            Written {
                path: "a/b/c.txt".into(),
                bytes: 3,
                created: true
            }
        );
        assert_eq!(std::fs::read(ws.join("a/b/c.txt")).unwrap(), b"one");
        let w = f.write("a/b/c.txt", b"two!").unwrap();
        assert!(!w.created);
        assert_eq!(f.read("a/b/c.txt").unwrap().text, "two!");
        f.write("empty", b"").unwrap();
        assert_eq!(std::fs::read(ws.join("empty")).unwrap(), b"");
        // No temporary file is left behind.
        assert!(
            std::fs::read_dir(ws.join("a/b"))
                .unwrap()
                .all(|e| e.unwrap().file_name() == "c.txt")
        );
        for bad in [
            "",
            ".",
            "/etc/x",
            "../x",
            "a/../../x",
            "C:/x",
            "a/con",
            "x:y",
        ] {
            assert!(
                matches!(f.write(bad, b"x"), Err(FolderError::InvalidPath(_))),
                "{bad}"
            );
        }
        assert!(matches!(f.write("a", b"x"), Err(FolderError::NotAFile)));
        assert!(matches!(
            f.write("empty/x", b"x"),
            Err(FolderError::NotADirectory)
        ));
        let big = vec![b'a'; MAX_WRITE_BYTES + 1];
        assert!(matches!(
            f.write("big", &big),
            Err(FolderError::Unsupported(_))
        ));
        assert!(f.write("max", &big[..MAX_WRITE_BYTES]).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn writes_never_follow_or_replace_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        let out = t.path().join("out");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("victim.txt"), "outside").unwrap();
        symlink(out.join("victim.txt"), ws.join("file_link")).unwrap();
        symlink(&out, ws.join("dir_link")).unwrap();
        symlink("missing-target", ws.join("dangling")).unwrap();
        let f = folder(&ws);
        for p in [
            "file_link",
            "dir_link/victim.txt",
            "dir_link/new.txt",
            "dangling",
        ] {
            let e = f.write(p, b"pwned").unwrap_err();
            assert!(
                matches!(e, FolderError::Link | FolderError::NotADirectory),
                "{p}: {e:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(out.join("victim.txt")).unwrap(),
            "outside"
        );
        assert!(!out.join("new.txt").exists());
        assert!(!out.join("missing-target").exists());
        assert!(
            std::fs::symlink_metadata(ws.join("file_link"))
                .unwrap()
                .is_symlink()
        );
        // A hard link to a file outside is replaced, never written through.
        std::fs::hard_link(out.join("victim.txt"), ws.join("hard")).unwrap();
        f.write("hard", b"new").unwrap();
        assert_eq!(std::fs::read(ws.join("hard")).unwrap(), b"new");
        assert_eq!(
            std::fs::read_to_string(out.join("victim.txt")).unwrap(),
            "outside"
        );
        // A replaced file keeps its mode.
        std::fs::write(ws.join("run.sh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(ws.join("run.sh"), std::fs::Permissions::from_mode(0o750))
            .unwrap();
        f.write("run.sh", b"#!/bin/sh\necho hi\n").unwrap();
        let mode = std::fs::metadata(ws.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o750);
        // A FIFO is not a file to replace.
        let fifo = std::ffi::CString::new(ws.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(matches!(f.write("pipe", b"x"), Err(FolderError::NotAFile)));
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_never_followed() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        let out = t.path().join("out");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("victim.txt"), "outside").unwrap();
        let made = std::process::Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(ws.join("j"))
            .arg(&out)
            .output()
            .is_ok_and(|o| o.status.success());
        if !made || !ws.join("j").exists() {
            eprintln!("skipped: junctions can't be made here");
            return;
        }
        let f = folder(&ws);
        for p in ["j/victim.txt", "j/new.txt"] {
            assert!(f.write(p, b"pwned").is_err(), "{p}");
            assert!(f.read(p).is_err(), "{p}");
        }
        assert!(f.list("j").is_err());
        assert_eq!(
            std::fs::read_to_string(out.join("victim.txt")).unwrap(),
            "outside"
        );
        assert!(!out.join("new.txt").exists());
        assert!(open_dir_path(&ws.join("j")).is_err());
        let limits = super::super::chosen::Limits {
            home: None,
            data_root: &t.path().join("data"),
        };
        let raw = ws.join("j").to_string_lossy().into_owned();
        assert_eq!(
            super::super::chosen::check(&raw, "s", &limits, &[])
                .unwrap_err()
                .code,
            "folder_is_link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn chosen_folder_open_refuses_links() {
        use std::os::unix::fs::symlink;
        let t = tempfile::tempdir().unwrap();
        let real = t.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::fs::write(t.path().join("file"), "x").unwrap();
        symlink(&real, t.path().join("link")).unwrap();
        assert!(open_dir_path(&real).is_ok());
        assert!(matches!(
            open_dir_path(&t.path().join("link")),
            Err(FolderError::Link | FolderError::NotADirectory)
        ));
        assert!(matches!(
            open_dir_path(&t.path().join("file")),
            Err(FolderError::NotADirectory)
        ));
        assert!(matches!(
            open_dir_path(&t.path().join("missing")),
            Err(FolderError::NotFound)
        ));
    }

    #[test]
    fn read_is_bounded() {
        let t = tempfile::tempdir().unwrap();
        let ws = t.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("big"), "a".repeat(MAX_READ_BYTES + 10)).unwrap();
        let c = folder(&ws).read("big").unwrap();
        assert!(c.truncated);
        assert_eq!(c.text.len(), MAX_READ_BYTES);
        assert_eq!(c.size, (MAX_READ_BYTES + 10) as u64);
    }
}
