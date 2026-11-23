//! Device-local operational errors. Neither the disk format nor the bridge
//! view accepts caller-provided text or identifiers.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

const LIMIT: usize = 100;
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    LauncherStartFailed,
    UiSpawnFailed,
    UiWaitFailed,
    UiExit127,
    UiMissingDll,
    UiUnexpectedExit,
    LocalServerStartFailed,
    LocalServerExited,
    DeviceWorkerOffline,
    DeviceWorkerFailed,
    DeviceWorkerUnauthorized,
    DeviceModelDiscoveryFailed,
}

impl EventKind {
    fn fields(self) -> (&'static str, &'static str, &'static str) {
        use EventKind::*;
        match self {
            LauncherStartFailed => (
                "launcher",
                "launcher_start_failed",
                "Metro could not start.",
            ),
            UiSpawnFailed => (
                "launcher",
                "ui_spawn_failed",
                "The Metro window could not start.",
            ),
            UiWaitFailed => (
                "launcher",
                "ui_wait_failed",
                "Metro could not read the window process status.",
            ),
            UiExit127 => (
                "launcher",
                "ui_exit_127",
                "The Metro window exited with status 127. A shared library may be missing.",
            ),
            UiMissingDll => (
                "launcher",
                "ui_missing_dll",
                "The Metro window could not load a required DLL.",
            ),
            UiUnexpectedExit => (
                "launcher",
                "ui_unexpected_exit",
                "The Metro window process exited unsuccessfully.",
            ),
            LocalServerStartFailed => (
                "local_server",
                "local_server_start_failed",
                "The local Metro server could not start.",
            ),
            LocalServerExited => (
                "local_server",
                "local_server_exited",
                "The local Metro server stopped unexpectedly.",
            ),
            DeviceWorkerOffline => (
                "device_connector",
                "device_worker_offline",
                "An on-device connection worker could not reach Metro; it will retry.",
            ),
            DeviceWorkerFailed => (
                "device_connector",
                "device_worker_failed",
                "An on-device connection worker stopped after an error.",
            ),
            DeviceWorkerUnauthorized => (
                "device_connector",
                "device_worker_unauthorized",
                "Metro stopped accepting an on-device connection worker.",
            ),
            DeviceModelDiscoveryFailed => (
                "device_connector",
                "device_model_discovery_failed",
                "An on-device connection could not list available models.",
            ),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    unix_seconds: u64,
    kind: EventKind,
}

#[derive(Serialize)]
pub struct Event {
    timestamp: String,
    component: &'static str,
    code: &'static str,
    summary: &'static str,
}

#[derive(Clone)]
pub struct Events(PathBuf);

impl Events {
    /// `root` is the desktop data directory, not a server data directory.
    pub fn new(root: &Path) -> Self {
        Self(root.join("monitoring"))
    }

    pub fn path(&self) -> PathBuf {
        self.0.join("events.json")
    }

    /// Best effort, including before the UI or its runtime can start.
    pub fn record(&self, kind: EventKind) {
        let _ = self.append(kind);
    }

    pub fn recent(&self) -> Result<Vec<Event>> {
        // A reader can use either snapshot during an atomic replacement.
        // Verify both directories even on the read-only path.
        if !private_dir(self.0.parent().expect("monitoring parent"), false, false)?
            || !private_dir(&self.0, false, true)?
        {
            return Ok(Vec::new());
        }
        Ok(self
            .read_snapshot(false)?
            .into_iter()
            .rev()
            .map(|r| {
                let (component, code, summary) = r.kind.fields();
                Event {
                    timestamp: timestamp(r.unix_seconds),
                    component,
                    code,
                    summary,
                }
            })
            .collect())
    }

    fn append(&self, kind: EventKind) -> Result<()> {
        if self.0.parent().is_none_or(|p| !p.is_absolute()) {
            bail!("monitoring root must be absolute");
        }
        private_dir(self.0.parent().expect("monitoring parent"), true, false)?;
        private_dir(&self.0, true, true)?;
        let lock_path = self.0.join("events.lock");
        check_file(&lock_path)?;
        let lock = private_options()
            .read(true)
            .write(true)
            .create(true)
            .open(lock_path)?;
        check_file_metadata(&lock.metadata()?)?;
        // Independent from the launcher instance lock: a second instance
        // that cannot open its UI can still record a failure.
        let mut retries = 10;
        loop {
            match lock.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock) if retries > 0 => {
                    retries -= 1;
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) => bail!("could not lock monitoring store: {e}"),
            }
        }

        let unix_seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let mut records = self.read_snapshot(true)?;
        records.push(Record { unix_seconds, kind });
        if records.len() > LIMIT {
            records.remove(0);
        }
        let bytes = serde_json::to_vec(&records)?;
        // Exactly one bounded staging file, even if a launcher is killed
        // before the rename. Never truncate an existing file in place.
        let temp = self.0.join("events.tmp");
        match fs::remove_file(&temp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut file = private_options().write(true).create_new(true).open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, self.path())?;
        drop(lock);
        Ok(())
    }

    // Writers can replace a malformed snapshot, but not an unsafe path.
    fn read_snapshot(&self, recover: bool) -> Result<Vec<Record>> {
        let path = self.path();
        if !check_file(&path)? {
            return Ok(Vec::new());
        }
        let file: File = private_options().read(true).open(path)?;
        check_file_metadata(&file.metadata()?)?;
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            if recover {
                return Ok(Vec::new());
            }
            bail!("monitoring snapshot exceeded its size limit");
        }
        let mut records: Vec<Record> = match serde_json::from_slice(&bytes) {
            Ok(records) => records,
            Err(_) if recover => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        records.retain(|r| r.unix_seconds <= 253_402_300_799); // 9999-12-31 UTC
        if records.len() > LIMIT {
            records.drain(..records.len() - LIMIT);
        }
        Ok(records)
    }
}

fn private_dir(path: &Path, create: bool, _monitoring_dir: bool) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => check_dir_metadata(&m)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(false),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            super::store::prepare_private_dir(path)?;
            check_dir_metadata(&fs::symlink_metadata(path)?)?;
        }
        Err(e) => return Err(e.into()),
    }
    // Ensure the store's newly created files inherit only user + SYSTEM.
    // Do not rewrite ACLs throughout an existing desktop root.
    #[cfg(windows)]
    if _monitoring_dir {
        super::store::protect_private_dir(path)?;
    }
    Ok(true)
}

fn check_dir_metadata(m: &fs::Metadata) -> Result<()> {
    if !m.is_dir() || is_link(m) {
        bail!("invalid monitoring directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o777 != 0o700 {
            bail!("monitoring directory must be private");
        }
    }
    Ok(())
}

fn check_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) => check_file_metadata(&m).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn check_file_metadata(m: &fs::Metadata) -> Result<()> {
    if !m.is_file() || is_link(m) {
        bail!("invalid monitoring file");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Zero is valid for an open snapshot that a concurrent writer
        // just replaced; more than one means an alias elsewhere.
        if m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o777 != 0o600 || m.nlink() > 1 {
            bail!("monitoring file must be private");
        }
    }
    Ok(())
}

fn is_link(m: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT also covers junctions.
        m.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    m.file_type().is_symlink()
}

fn private_options() -> OpenOptions {
    let mut opts = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT; metadata checks the opened object.
        opts.custom_flags(0x0020_0000);
    }
    opts
}

/// UTC Gregorian date from Unix seconds, with March as the first month of a
/// 400-year era so leap days are grouped at the end of each year.
fn timestamp(seconds: u64) -> String {
    let days = seconds / 86_400 + 719_468;
    let era = days / 146_097;
    let day_of_era = days % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let march_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * march_month + 2) / 5 + 1;
    let month = (march_month + 2) % 12 + 1;
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    let hour = seconds / 3600 % 24;
    let minute = seconds / 60 % 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_reopens_private_allowlisted_snapshots() {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().join("desktop");
        let events = Events::new(&root);
        assert!(events.recent().unwrap().is_empty());
        events.append(EventKind::UiExit127).unwrap();
        for _ in 0..99 {
            events.append(EventKind::DeviceWorkerFailed).unwrap();
        }
        events.append(EventKind::LocalServerExited).unwrap();

        let events = Events::new(&root);
        let recent = serde_json::to_value(events.recent().unwrap()).unwrap();
        let list = recent.as_array().unwrap();
        assert_eq!(list.len(), 100);
        assert_eq!(list[0]["code"], "local_server_exited");
        assert_eq!(list[99]["code"], "device_worker_failed");
        assert_eq!(list[0].as_object().unwrap().len(), 4);
        assert!(list[0]["timestamp"].as_str().unwrap().ends_with('Z'));
        assert!(fs::metadata(events.path()).unwrap().len() <= MAX_BYTES);
        assert!(!events.0.join("events.tmp").exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            for dir in [&root, &events.0] {
                assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
            }
            for file in [events.path(), events.0.join("events.lock")] {
                assert_eq!(fs::metadata(file).unwrap().mode() & 0o777, 0o600);
            }
        }
    }

    #[test]
    fn refuses_unbounded_or_unrecognized_content() {
        let t = tempfile::tempdir().unwrap();
        let events = Events::new(&t.path().join("desktop"));
        events.append(EventKind::UiExit127).unwrap();
        fs::write(
            events.path(),
            br#"[{"unix_seconds":42,"kind":"ui_exit_127","summary":"not allowlisted"}]"#,
        )
        .unwrap();
        assert!(events.recent().is_err());
        fs::write(events.path(), vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(events.recent().is_err());
        events.append(EventKind::LocalServerExited).unwrap();
        assert_eq!(events.recent().unwrap()[0].code, "local_server_exited");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_nonprivate_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let t = tempfile::tempdir().unwrap();
        let events = Events::new(&t.path().join("desktop"));
        events.append(EventKind::UiExit127).unwrap();
        let path = events.path();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(events.recent().is_err());
        assert!(events.append(EventKind::LauncherStartFailed).is_err());

        let target = t.path().join("unmodified");
        fs::rename(&path, &target).unwrap();
        symlink(&target, path).unwrap();
        let original = fs::read(&target).unwrap();
        assert!(events.recent().is_err());
        assert!(events.append(EventKind::LauncherStartFailed).is_err());
        assert_eq!(fs::read(target).unwrap(), original);
    }

    #[test]
    fn formats_rfc3339_utc() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(951_782_463), "2000-02-29T00:01:03Z");
        assert_eq!(timestamp(4_107_542_400), "2100-03-01T00:00:00Z");
        assert_eq!(timestamp(253_402_300_799), "9999-12-31T23:59:59Z");
    }
}
