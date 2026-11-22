//! Per-Space pairing records and folders under the desktop's private
//! local-tools root.
//!
//! ```text
//! <root>/                                   private (700; Windows: the desktop root's DACL)
//!   accounts/<sha256(account id)[..32]>/
//!     spaces/<space id>/
//!       pairing.json                        600: the Space's dedicated runner token + ETag
//!       workspace/                          the only folder the tools read
//! ```
//!
//! The root is opened once as a directory handle that is proven to be a real
//! directory (not a symlink, junction or other reparse point). Everything
//! below it is created, opened, renamed and removed relative to handles with
//! links never followed, so nothing here can write outside the root, and a
//! link planted inside it can't make one Space's tools read another's folder
//! or a pairing file.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use cap_primitives::fs::{DirOptions, FollowSymlinks, OpenOptions};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use super::folder::{FolderError, open_file, open_subdir};
use crate::Secret;

const ACCOUNTS: &str = "accounts";
const SPACES: &str = "spaces";
const PAIRING: &str = "pairing.json";
const PAIRING_TMP: &str = "pairing.json.tmp";
pub const WORKSPACE: &str = "workspace";
const MAX_PAIRING_BYTES: u64 = 16 * 1024;

/// Whether the server still accepts this pairing, as far as we know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingState {
    Active,
    /// The server refused the token (revoked or replaced). Never re-issued
    /// automatically; the owner turns local tools on again explicitly.
    Revoked,
}

impl PairingState {
    fn as_str(self) -> &'static str {
        match self {
            PairingState::Active => "active",
            PairingState::Revoked => "revoked",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Pairing {
    pub account_id: String,
    pub space_id: String,
    /// The guarded-issue ETag: revoking uses `If-Match` with it, so this
    /// desktop can only ever revoke the exact token it was given.
    pub etag: String,
    pub token: Secret,
    pub state: PairingState,
    pub enabled_at_ms: u64,
}

/// Space ids name a directory: the same conservative slug the runner uses
/// (`[a-z0-9][a-z0-9_-]{0,63}`), lowercase only so case-insensitive file
/// systems can't alias two Spaces.
pub fn valid_space_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_'))
}

/// Directory name for an account: a hash, so any id shape is safe.
pub fn account_dir(account_id: &str) -> String {
    let digest = Sha256::digest(account_id.as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

fn io_err(e: FolderError) -> io::Error {
    match e {
        FolderError::Io(e) => e,
        FolderError::NotFound => io::Error::new(io::ErrorKind::NotFound, "not found"),
        other => io::Error::other(other.to_string()),
    }
}

/// Open `path` as the local-tools root: create it private if missing, then
/// open it and prove the handle is a plain directory.
pub fn open_root(path: &Path) -> io::Result<File> {
    crate::fsutil::ensure_private_dir(path).map_err(|e| io::Error::other(format!("{e:#}")))?;
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
    let f = opts.open(path)?;
    let meta = f.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::other(
                "the local tools folder is a link or junction; refusing to use it",
            ));
        }
    }
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other(
            "the local tools folder is not a plain directory",
        ));
    }
    Ok(f)
}

/// Open (creating if `create`) `name` inside `dir`, never following links.
fn subdir(dir: &File, name: &str, create: bool) -> io::Result<File> {
    match open_subdir(dir, name) {
        Ok(f) => Ok(f),
        Err(FolderError::NotFound) if create => {
            #[cfg_attr(not(unix), allow(unused_mut))]
            let mut o = DirOptions::new();
            #[cfg(unix)]
            {
                use cap_primitives::fs::DirBuilderExt;
                o.mode(0o700);
            }
            match cap_primitives::fs::create_dir(dir, Path::new(name), &o) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e),
            }
            open_subdir(dir, name).map_err(io_err)
        }
        Err(e) => Err(io_err(e)),
    }
}

/// The handle of `spaces/<sid>/` for this account.
fn space_dir(root: &File, account_id: &str, sid: &str, create: bool) -> io::Result<File> {
    if !valid_space_id(sid) {
        return Err(io::Error::other("invalid Space id"));
    }
    let a = subdir(root, ACCOUNTS, create)?;
    let a = subdir(&a, &account_dir(account_id), create)?;
    let s = subdir(&a, SPACES, create)?;
    subdir(&s, sid, create)
}

/// The Space's folder, created if needed. Opened without following links.
pub fn workspace(root: &File, account_id: &str, sid: &str, create: bool) -> io::Result<File> {
    let s = space_dir(root, account_id, sid, create)?;
    subdir(&s, WORKSPACE, create)
}

/// Where the Space's folder is, for showing to its owner.
pub fn workspace_path(root_path: &Path, account_id: &str, sid: &str) -> PathBuf {
    root_path
        .join(ACCOUNTS)
        .join(account_dir(account_id))
        .join(SPACES)
        .join(sid)
        .join(WORKSPACE)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Pairing {
    pub fn new(account_id: &str, space_id: &str, etag: String, token: Secret) -> Self {
        Pairing {
            account_id: account_id.to_owned(),
            space_id: space_id.to_owned(),
            etag,
            token,
            state: PairingState::Active,
            enabled_at_ms: now_ms(),
        }
    }

    fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "v": 1,
            "account_id": self.account_id,
            "space_id": self.space_id,
            "etag": self.etag,
            "token": self.token.expose(),
            "state": self.state.as_str(),
            "enabled_at_ms": self.enabled_at_ms,
        }))
        .expect("serializable")
    }

    fn from_json(bytes: &[u8]) -> Option<Self> {
        let mut v: Value = serde_json::from_slice(bytes).ok()?;
        let o = v.as_object_mut()?;
        if o.get("v")?.as_u64()? != 1 {
            return None;
        }
        let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_owned);
        let state = match s("state")?.as_str() {
            "active" => PairingState::Active,
            "revoked" => PairingState::Revoked,
            _ => return None,
        };
        let (account_id, space_id, etag) = (s("account_id")?, s("space_id")?, s("etag")?);
        let enabled_at_ms = o.get("enabled_at_ms").and_then(Value::as_u64).unwrap_or(0);
        let Some(Value::String(t)) = o.get_mut("token") else {
            return None;
        };
        let token = Secret::new(t.as_str());
        t.zeroize();
        Some(Pairing {
            account_id,
            space_id,
            etag,
            token,
            state,
            enabled_at_ms,
        })
    }
}

/// The Space's pairing, if any. A record for another account or Space (a
/// copied or tampered file) is treated as absent.
pub fn load(root: &File, account_id: &str, sid: &str) -> io::Result<Option<Pairing>> {
    let dir = match space_dir(root, account_id, sid, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let file = match open_file(&dir, PAIRING) {
        Ok(f) => f,
        Err(FolderError::NotFound) => return Ok(None),
        Err(e) => return Err(io_err(e)),
    };
    let mut buf = Vec::new();
    file.take(MAX_PAIRING_BYTES + 1).read_to_end(&mut buf)?;
    let parsed = if buf.len() as u64 > MAX_PAIRING_BYTES {
        None
    } else {
        Pairing::from_json(&buf)
    };
    buf.zeroize();
    Ok(parsed.filter(|p| p.account_id == account_id && p.space_id == sid))
}

/// Write the Space's pairing atomically (new file, fsync, rename), mode 600.
pub fn save(root: &File, p: &Pairing) -> io::Result<()> {
    let dir = space_dir(root, &p.account_id, &p.space_id, true)?;
    let _ = cap_primitives::fs::remove_file(&dir, Path::new(PAIRING_TMP));
    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    opts._cap_fs_ext_follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_primitives::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut data = p.to_json();
    let written = (|| -> io::Result<()> {
        let mut f = cap_primitives::fs::open(&dir, Path::new(PAIRING_TMP), &opts)?;
        f.write_all(&data)?;
        f.sync_all()?;
        Ok(())
    })();
    data.zeroize();
    if let Err(e) = written {
        let _ = cap_primitives::fs::remove_file(&dir, Path::new(PAIRING_TMP));
        return Err(e);
    }
    if let Err(e) =
        cap_primitives::fs::rename(&dir, Path::new(PAIRING_TMP), &dir, Path::new(PAIRING))
    {
        let _ = cap_primitives::fs::remove_file(&dir, Path::new(PAIRING_TMP));
        return Err(e);
    }
    Ok(())
}

/// Delete the Space's pairing (its token). The folder and its files stay:
/// they are the person's own files.
pub fn remove(root: &File, account_id: &str, sid: &str) -> io::Result<()> {
    let dir = match space_dir(root, account_id, sid, false) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for name in [PAIRING_TMP, PAIRING] {
        match cap_primitives::fs::remove_file(&dir, Path::new(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Every Space of this account that has a pairing record.
pub fn list(root: &File, account_id: &str) -> io::Result<Vec<Pairing>> {
    let spaces = match subdir(root, ACCOUNTS, false)
        .and_then(|a| subdir(&a, &account_dir(account_id), false))
        .and_then(|a| subdir(&a, SPACES, false))
    {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let dir = cap_std::fs::Dir::from_std_file(spaces);
    let mut out = Vec::new();
    for ent in dir.entries()? {
        let ent = ent?;
        let Ok(name) = ent.file_name().into_string() else {
            continue;
        };
        if !valid_space_id(&name) || !ent.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if let Ok(Some(p)) = load(root, account_id, &name) {
            out.push(p);
        }
        if out.len() >= 1000 {
            break;
        }
    }
    out.sort_by(|a, b| a.space_id.cmp(&b.space_id));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_isolation() {
        let t = tempfile::tempdir().unwrap();
        let root_path = t.path().join("local-tools");
        let root = open_root(&root_path).unwrap();
        let p = Pairing::new(
            "alice",
            "s-a",
            "\"rtg-1\"".into(),
            Secret::new("metro_rt_x"),
        );
        save(&root, &p).unwrap();
        let got = load(&root, "alice", "s-a").unwrap().unwrap();
        assert_eq!(got.token.expose(), "metro_rt_x");
        assert_eq!(got.state, PairingState::Active);
        // Another account or Space sees nothing.
        assert!(load(&root, "bob", "s-a").unwrap().is_none());
        assert!(load(&root, "alice", "s-b").unwrap().is_none());
        assert_eq!(list(&root, "alice").unwrap().len(), 1);
        assert!(list(&root, "bob").unwrap().is_empty());
        workspace(&root, "alice", "s-a", true).unwrap();
        assert!(
            workspace_path(&root_path, "alice", "s-a").is_dir(),
            "folder created where the path says"
        );
        remove(&root, "alice", "s-a").unwrap();
        assert!(load(&root, "alice", "s-a").unwrap().is_none());
        assert!(
            workspace_path(&root_path, "alice", "s-a").is_dir(),
            "files kept"
        );
        assert!(
            save(
                &root,
                &Pairing::new("a", "../x", String::new(), Secret::new("t"))
            )
            .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_and_no_links() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let t = tempfile::tempdir().unwrap();
        let root_path = t.path().join("local-tools");
        let root = open_root(&root_path).unwrap();
        let p = Pairing::new(
            "alice",
            "s-a",
            "\"rtg-1\"".into(),
            Secret::new("metro_rt_x"),
        );
        save(&root, &p).unwrap();
        let sdir = root_path
            .join(ACCOUNTS)
            .join(account_dir("alice"))
            .join(SPACES)
            .join("s-a");
        let mode = std::fs::metadata(sdir.join(PAIRING))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let mode = std::fs::metadata(&sdir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
        // A Space folder that is a link to a sibling Space is refused.
        workspace(&root, "alice", "s-a", true).unwrap();
        let bdir = sdir.parent().unwrap().join("s-b");
        std::fs::create_dir(&bdir).unwrap();
        symlink(sdir.join(WORKSPACE), bdir.join(WORKSPACE)).unwrap();
        assert!(workspace(&root, "alice", "s-b", false).is_err());
        assert!(workspace(&root, "alice", "s-b", true).is_err());
        // A root that is a symlink is refused.
        symlink(&root_path, t.path().join("root-link")).unwrap();
        assert!(open_root(&t.path().join("root-link")).is_err());
    }
}
