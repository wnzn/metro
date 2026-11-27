//! A folder the person chose for one Space of this computer's runner.
//!
//! A choice must be an existing absolute directory that is not itself a
//! link or reparse point, not a filesystem root, not the home folder itself,
//! not inside (or around) Metro's own data folder, and must not equal,
//! contain or be inside the folder chosen for another Space of the same
//! runner record. Paths are compared after canonicalisation, and without
//! case on Windows and macOS. The canonical path is what gets stored. The
//! same checks run again before each job ([`recheck`]).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const MAX_PATH_BYTES: usize = 4096;

/// Why a folder can't be used; `code` is the bridge error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
    }
}

/// What a choice is checked against.
pub struct Limits<'a> {
    /// The person's home folder, if known.
    pub home: Option<PathBuf>,
    /// Metro's own data folder (the desktop root).
    pub data_root: &'a Path,
}

impl Limits<'_> {
    /// The home folder from the environment.
    pub fn home_from_env() -> Option<PathBuf> {
        #[cfg(windows)]
        let v = std::env::var_os("USERPROFILE");
        #[cfg(not(windows))]
        let v = std::env::var_os("HOME");
        v.filter(|h| !h.is_empty()).map(PathBuf::from)
    }
}

/// `\\?\C:\x` → `C:\x` and `\\?\UNC\h\s` → `\\h\s`, so stored and shown
/// paths look like the ones the person typed. Other forms stay as they are.
fn simplify(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            let b = rest.as_bytes();
            if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\' {
                return PathBuf::from(rest);
            }
        }
    }
    p
}

/// The canonical form of `p`, simplified as above.
pub fn canonical(p: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(p).map(simplify)
}

/// Comparable form: case folded where the usual file systems ignore case.
fn fold(p: &Path) -> PathBuf {
    if cfg!(any(windows, target_os = "macos")) {
        PathBuf::from(p.to_string_lossy().to_lowercase())
    } else {
        p.to_path_buf()
    }
}

fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (fold(a), fold(b));
    a.starts_with(&b) || b.starts_with(&a)
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

fn plain_dir(p: &Path) -> Result<(), Refusal> {
    match fs::symlink_metadata(p) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(refuse("folder_missing", "That folder doesn't exist."))
        }
        Err(_) => Err(refuse("folder_missing", "Metro can't open that folder.")),
        Ok(m) if is_link(&m) => Err(refuse(
            "folder_is_link",
            "That folder is a link or junction. Choose the folder it points to.",
        )),
        Ok(m) if !m.is_dir() => Err(refuse("folder_not_directory", "That isn't a folder.")),
        Ok(_) => Ok(()),
    }
}

/// Check `raw` for Space `sid`; `others` are the other Spaces' chosen
/// folders of the same record. Returns the canonical path to store.
pub fn check(
    raw: &str,
    sid: &str,
    limits: &Limits<'_>,
    others: &[(String, PathBuf)],
) -> Result<PathBuf, Refusal> {
    if raw.is_empty() || raw.len() > MAX_PATH_BYTES || raw.chars().any(char::is_control) {
        return Err(refuse("folder_not_absolute", "Enter a full folder path."));
    }
    let given = Path::new(raw);
    if !given.is_absolute() {
        return Err(refuse(
            "folder_not_absolute",
            "Enter a full folder path, not a relative one.",
        ));
    }
    plain_dir(given)?;
    let canon =
        canonical(given).map_err(|_| refuse("folder_missing", "Metro can't open that folder."))?;
    if canon.to_str().is_none() {
        return Err(refuse(
            "folder_unsupported",
            "That folder's path has characters Metro can't store.",
        ));
    }
    plain_dir(&canon)?;
    within_limits(&canon, sid, limits, others)?;
    Ok(canon)
}

fn within_limits(
    canon: &Path,
    sid: &str,
    limits: &Limits<'_>,
    others: &[(String, PathBuf)],
) -> Result<(), Refusal> {
    if canon.parent().is_none() {
        return Err(refuse(
            "folder_is_root",
            "Choose a folder, not the root of a drive or file system.",
        ));
    }
    if let Some(home) = limits.home.as_deref()
        && fold(&canonical(home).unwrap_or_else(|_| home.to_path_buf())) == fold(canon)
    {
        return Err(refuse(
            "folder_is_home",
            "Choose a folder inside your home folder, not the home folder itself.",
        ));
    }
    let data = canonical(limits.data_root).unwrap_or_else(|_| limits.data_root.to_path_buf());
    if overlaps(canon, &data) {
        return Err(refuse(
            "folder_in_app_data",
            "That folder is inside or contains Metro's own data folder.",
        ));
    }
    for (other, p) in others.iter().filter(|(o, _)| o != sid) {
        let p = canonical(p).unwrap_or_else(|_| p.clone());
        if overlaps(canon, &p) {
            return Err(refuse(
                "folder_overlaps",
                format!(
                    "That folder is the same as, inside or around the folder of Space {other}."
                ),
            ));
        }
    }
    Ok(())
}

/// Before a job: the stored choice must still be a plain directory, still
/// canonical (nothing on its path became a link) and still within limits.
pub fn recheck(
    stored: &Path,
    sid: &str,
    limits: &Limits<'_>,
    others: &[(String, PathBuf)],
) -> Result<(), Refusal> {
    plain_dir(stored)?;
    let canon =
        canonical(stored).map_err(|_| refuse("folder_missing", "Metro can't open that folder."))?;
    if fold(&canon) != fold(&simplify(stored.to_path_buf())) {
        return Err(refuse(
            "folder_is_link",
            "Part of that folder's path is now a link or junction.",
        ));
    }
    within_limits(&canon, sid, limits, others)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Env {
        _t: tempfile::TempDir,
        base: PathBuf,
        data: PathBuf,
        home: PathBuf,
    }

    fn env() -> Env {
        let t = tempfile::tempdir().unwrap();
        let base = canonical(t.path()).unwrap();
        let (data, home) = (base.join("appdata"), base.join("home"));
        for d in [&data, &data.join("local-tools"), &home, &home.join("p1")] {
            fs::create_dir_all(d).unwrap();
        }
        Env {
            _t: t,
            base,
            data,
            home,
        }
    }

    fn limits(e: &Env) -> Limits<'_> {
        Limits {
            home: Some(e.home.clone()),
            data_root: &e.data,
        }
    }

    fn code(r: Result<PathBuf, Refusal>) -> &'static str {
        r.unwrap_err().code
    }

    #[test]
    fn accepts_a_plain_folder_and_stores_it_canonical() {
        let e = env();
        let p = e.home.join("p1");
        let raw = format!("{}/./", p.display());
        assert_eq!(check(&raw, "s-a", &limits(&e), &[]).unwrap(), p);
    }

    #[test]
    fn refuses_what_is_not_allowed() {
        let e = env();
        let l = limits(&e);
        fs::write(e.home.join("file"), "x").unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(code(check("", "s", &l, &[])), "folder_not_absolute");
        assert_eq!(code(check("p1", "s", &l, &[])), "folder_not_absolute");
        assert_eq!(code(check("./p1", "s", &l, &[])), "folder_not_absolute");
        assert_eq!(
            code(check(&s(&e.home.join("nope")), "s", &l, &[])),
            "folder_missing"
        );
        assert_eq!(
            code(check(&s(&e.home.join("file")), "s", &l, &[])),
            "folder_not_directory"
        );
        let root = if cfg!(windows) { r"C:\" } else { "/" };
        assert_eq!(code(check(root, "s", &l, &[])), "folder_is_root");
        assert_eq!(code(check(&s(&e.home), "s", &l, &[])), "folder_is_home");
        assert_eq!(
            code(check(&s(&e.data.join("local-tools")), "s", &l, &[])),
            "folder_in_app_data"
        );
        assert_eq!(code(check(&s(&e.data), "s", &l, &[])), "folder_in_app_data");
        // Containing the data folder is refused too.
        assert_eq!(code(check(&s(&e.base), "s", &l, &[])), "folder_in_app_data");
        // `..` is resolved before comparing.
        let sneaky = format!("{}/../appdata/local-tools", e.home.display());
        assert_eq!(code(check(&sneaky, "s", &l, &[])), "folder_in_app_data");
    }

    #[test]
    fn other_spaces_folders_never_overlap() {
        let e = env();
        let l = limits(&e);
        let a = e.home.join("p1");
        fs::create_dir_all(a.join("inner")).unwrap();
        fs::create_dir_all(e.home.join("p2")).unwrap();
        fs::create_dir_all(e.home.join("p10")).unwrap();
        let others = vec![("s-a".to_string(), a.clone())];
        let s = |p: &Path| p.to_string_lossy().into_owned();
        for p in [a.clone(), a.join("inner")] {
            assert_eq!(code(check(&s(&p), "s-b", &l, &others)), "folder_overlaps");
        }
        // Around it: also refused.
        let wide = e.base.join("w");
        fs::create_dir_all(wide.join("x")).unwrap();
        let others_w = vec![("s-a".to_string(), wide.join("x"))];
        assert_eq!(
            code(check(&s(&wide), "s-b", &l, &others_w)),
            "folder_overlaps"
        );
        // A sibling, and a name that only shares a prefix, are fine.
        assert!(check(&s(&e.home.join("p2")), "s-b", &l, &others).is_ok());
        assert!(check(&s(&e.home.join("p10")), "s-b", &l, &others).is_ok());
        // The Space's own earlier choice doesn't count against it.
        assert!(check(&s(&a), "s-a", &l, &others).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn links_are_refused_and_compared_resolved() {
        use std::os::unix::fs::symlink;
        let e = env();
        let l = limits(&e);
        let a = e.home.join("p1");
        symlink(&a, e.home.join("link")).unwrap();
        symlink(&e.data, e.home.join("data-link")).unwrap();
        let s = |p: &Path| p.to_string_lossy().into_owned();
        assert_eq!(
            code(check(&s(&e.home.join("link")), "s", &l, &[])),
            "folder_is_link"
        );
        // Through a linked parent: resolved, then compared.
        fs::create_dir(e.data.join("x")).unwrap();
        assert_eq!(
            code(check(&s(&e.home.join("data-link/x")), "s", &l, &[])),
            "folder_in_app_data"
        );
        fs::create_dir(a.join("sub")).unwrap();
        let others = vec![("s-a".to_string(), a.clone())];
        assert_eq!(
            code(check(&s(&e.home.join("link/sub")), "s-b", &l, &others)),
            "folder_overlaps"
        );
        // Stored before, a link now: the job-time check refuses it.
        let p = e.home.join("later");
        fs::create_dir(&p).unwrap();
        assert!(recheck(&p, "s", &l, &[]).is_ok());
        fs::remove_dir(&p).unwrap();
        symlink(&a, &p).unwrap();
        assert_eq!(
            recheck(&p, "s", &l, &[]).unwrap_err().code,
            "folder_is_link"
        );
        fs::remove_file(&p).unwrap();
        assert_eq!(
            recheck(&p, "s", &l, &[]).unwrap_err().code,
            "folder_missing"
        );
    }
}
