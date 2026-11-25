use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::path::{Component, Path};

use rustix::fs::{CWD, Mode, OFlags, mkdirat, openat};

pub const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
pub const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);

/// Pin every directory in the path before looking up the next component.
pub fn open_dir(path: &Path) -> io::Result<File> {
    let start = if path.is_absolute() { "/" } else { "." };
    let mut dir = File::from(openat(CWD, start, DIRECTORY, Mode::empty())?);
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(name) => dir = child_dir(&dir, name, false)?,
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        }
    }
    Ok(dir)
}

pub fn child_dir(dir: &File, name: &OsStr, create: bool) -> io::Result<File> {
    let open = || openat(dir, name, DIRECTORY, Mode::empty()).map(File::from);
    match open() {
        Err(rustix::io::Errno::NOENT) if create => {
            match mkdirat(dir, name, Mode::RUSR | Mode::WUSR | Mode::XUSR) {
                Ok(()) => dir.sync_all()?,
                Err(rustix::io::Errno::EXIST) => {}
                Err(e) => return Err(e.into()),
            }
            Ok(open()?)
        }
        result => Ok(result?),
    }
}

pub fn empty(dir: &File) -> io::Result<bool> {
    for entry in rustix::fs::Dir::read_from(dir)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(feature = "async")]
pub fn write_atomic(dir: &File, name: &str, bytes: &[u8]) -> io::Result<()> {
    use rustix::fs::{AtFlags, renameat, unlinkat};
    use std::io::Write;
    let tmp = crate::atomic_temp_name(name);
    let opts = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut file = File::from(openat(dir, tmp.as_str(), opts, Mode::RUSR | Mode::WUSR)?);
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        renameat(dir, tmp.as_str(), dir, name)?;
        dir.sync_all()
    })();
    if result.is_err() {
        let _ = unlinkat(dir, tmp.as_str(), AtFlags::empty());
    }
    result
}
