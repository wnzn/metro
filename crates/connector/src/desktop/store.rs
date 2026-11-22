//! Where the desktop app keeps its on-device connections.
//!
//! ```text
//! <desktop root>/                 mode 700 (Windows: protected DACL, user + SYSTEM)
//!   instance.lock                 held by the primary app instance
//!   profiles/<c…>/                a metro-connector profile (same format)
//!     desktop.json                the binding: server, account, connection, kind
//!   local/                        the local workspace's data (desktop_local)
//! ```
//!
//! A profile's name is derived from (server URL, account id, connection id),
//! so one account's connections never collide with, or get served for,
//! another account on the same machine.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fsutil::{ensure_private_dir, read_private, write_private};
use crate::profile::{Kind, Profile};

pub const ENV_HOME: &str = "METRO_DESKTOP_HOME";
const BINDING: &str = "desktop.json";

/// The desktop root: `METRO_DESKTOP_HOME` (absolute) or the platform's
/// per-user, non-roaming application data directory.
pub fn desktop_root() -> Result<PathBuf> {
    if let Some(h) = std::env::var_os(ENV_HOME).filter(|v| !v.is_empty()) {
        let p = PathBuf::from(h);
        if !p.is_absolute() {
            bail!("{ENV_HOME} must be an absolute path");
        }
        return Ok(p);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .context("LOCALAPPDATA is not set")?;
        Ok(base.join("Metro").join("Desktop"))
    }
    #[cfg(not(windows))]
    {
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        #[cfg(target_os = "macos")]
        {
            let home = home.context("HOME is not set")?;
            Ok(home.join("Library/Application Support/Metro/Desktop"))
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(x) = std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
            {
                return Ok(x.join("metro-desktop"));
            }
            Ok(home
                .context("HOME is not set")?
                .join(".config")
                .join("metro-desktop"))
        }
    }
}

/// Create (or check) a private directory: mode 700 owned by this user on
/// Unix; on Windows a protected DACL granting only this user and SYSTEM, so
/// inherited ACLs of the parent can't widen access.
pub fn prepare_private_dir(dir: &Path) -> Result<()> {
    ensure_private_dir(dir)?;
    #[cfg(windows)]
    win::protect_dir(dir).with_context(|| format!("securing {}", dir.display()))?;
    Ok(())
}

/// Held for the life of the primary instance; released when dropped or
/// when the process ends.
pub struct InstanceLock {
    _file: File,
}

/// Try to become the primary instance. `Ok(None)`: another instance holds it.
pub fn try_lock_instance(root: &Path) -> Result<Option<InstanceLock>> {
    let path = root.join("instance.lock");
    if let Ok(m) = std::fs::symlink_metadata(&path)
        && !m.is_file()
    {
        bail!("{} is not a regular file", path.display());
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(InstanceLock { _file: file })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// A desktop profile's binding to exactly one Metro connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub server_url: String,
    pub account_id: String,
    pub connection_id: String,
    pub kind: Kind,
    /// Endpoint kinds: the person chose to send no key.
    #[serde(default)]
    pub keyless: bool,
}

/// The profile name for (server, account, connection).
pub fn profile_name(server_url: &str, account_id: &str, connection_id: &str) -> String {
    let mut h = Sha256::new();
    for part in [server_url, account_id, connection_id] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    let d = h.finalize();
    let mut s = String::from("c");
    for b in &d[..12] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn profile_for(root: &Path, server_url: &str, account_id: &str, id: &str) -> Result<Profile> {
    Profile::open(root, &profile_name(server_url, account_id, id))
}

pub fn load_binding(p: &Profile) -> Result<Option<Binding>> {
    let Some(bytes) = read_private(&p.dir.join(BINDING), 16 * 1024)? else {
        return Ok(None);
    };
    let b: Binding = serde_json::from_slice(&bytes).context("desktop.json is not valid")?;
    if b.version != 1 {
        bail!("desktop.json: unsupported version");
    }
    Ok(Some(b))
}

pub fn save_binding(p: &Profile, b: &Binding) -> Result<()> {
    p.ensure()?;
    let mut json = serde_json::to_vec_pretty(b)?;
    json.push(b'\n');
    write_private(&p.dir.join(BINDING), &json)
}

/// A connection id goes into a profile binding and a URL path: a
/// conservative charset only.
pub fn valid_connection_id(id: &str) -> bool {
    crate::metro::valid_job_id(id)
}

/// The bound profiles of (server, account), sorted by connection id. The
/// binding must match the profile name it lives under, so a copied or
/// edited directory is never served for someone else.
pub fn bound_profiles(root: &Path, server_url: &str, account_id: &str) -> Vec<(Profile, Binding)> {
    let mut out = Vec::new();
    let Ok(names) = crate::profile::list_profiles(root) else {
        return out;
    };
    for name in names {
        let Ok(p) = Profile::open(root, &name) else {
            continue;
        };
        if let Ok(Some(b)) = load_binding(&p)
            && b.server_url == server_url
            && b.account_id == account_id
            && profile_name(&b.server_url, &b.account_id, &b.connection_id) == name
        {
            out.push((p, b));
        }
    }
    out.sort_by(|a, b| a.1.connection_id.cmp(&b.1.connection_id));
    out
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use anyhow::{Result, bail};
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn current_user_sid() -> Result<String> {
        // SAFETY: plain Win32 calls with checked results; buffers are sized
        // by the first GetTokenInformation call; handles are closed.
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                bail!("OpenProcessToken failed");
            }
            let mut len = 0u32;
            GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut len);
            let mut buf = vec![0u64; (len as usize).div_ceil(8).max(1)];
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), len, &mut len);
            CloseHandle(token);
            if ok == 0 {
                bail!("GetTokenInformation failed");
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut s) == 0 {
                bail!("ConvertSidToStringSidW failed");
            }
            let mut n = 0;
            while *s.add(n) != 0 {
                n += 1;
            }
            let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
            LocalFree(s.cast());
            Ok(out)
        }
    }

    /// Replace the directory's DACL with a protected one: full control for
    /// the current user and SYSTEM, inherited by everything created inside.
    pub fn protect_dir(dir: &Path) -> Result<()> {
        let sid = current_user_sid()?;
        if !sid.starts_with("S-1-") || !sid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            bail!("unexpected user SID");
        }
        let sddl = wide(std::ffi::OsStr::new(&format!(
            "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"
        )));
        let path = wide(dir.as_os_str());
        // SAFETY: the SDDL and path are NUL-terminated; the descriptor is
        // freed with LocalFree; the DACL pointer lives inside it.
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            ) == 0
            {
                bail!("cannot build the security descriptor");
            }
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl: *mut ACL = std::ptr::null_mut();
            let got = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted);
            let rc = if got != 0 && present != 0 {
                SetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    dacl,
                    std::ptr::null(),
                )
            } else {
                1
            };
            LocalFree(sd);
            if rc != 0 {
                bail!("SetNamedSecurityInfoW failed ({rc})");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_per_account_and_valid() {
        let a = profile_name("https://m.example", "alice", "c1");
        let b = profile_name("https://m.example", "bob", "c1");
        let c = profile_name("https://other.example", "alice", "c1");
        assert_ne!(a, b);
        assert_ne!(a, c);
        // Length-prefixed: no ambiguity from shifting separators.
        assert_ne!(profile_name("a", "bc", "d"), profile_name("ab", "c", "d"));
        crate::profile::validate_profile_name(&a).unwrap();
        assert_eq!(a.len(), 25);
    }

    #[test]
    fn instance_lock_is_exclusive() {
        let t = tempfile::tempdir().unwrap();
        let first = try_lock_instance(t.path()).unwrap();
        assert!(first.is_some());
        assert!(try_lock_instance(t.path()).unwrap().is_none());
        drop(first);
        assert!(try_lock_instance(t.path()).unwrap().is_some());
    }

    #[test]
    fn bindings_must_match_their_directory() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path();
        let p = profile_for(root, "https://m.example", "alice", "c1").unwrap();
        let b = Binding {
            version: 1,
            server_url: "https://m.example".into(),
            account_id: "alice".into(),
            connection_id: "c1".into(),
            kind: Kind::OpenAiCompatible,
            keyless: false,
        };
        save_binding(&p, &b).unwrap();
        std::fs::write(p.dir.join("profile.json"), "{}").unwrap();
        assert_eq!(bound_profiles(root, "https://m.example", "alice").len(), 1);
        assert!(bound_profiles(root, "https://m.example", "bob").is_empty());
        // A binding copied under another account's name is ignored.
        let q = profile_for(root, "https://m.example", "bob", "c1").unwrap();
        save_binding(&q, &b).unwrap();
        std::fs::write(q.dir.join("profile.json"), "{}").unwrap();
        assert_eq!(bound_profiles(root, "https://m.example", "alice").len(), 1);
    }
}
