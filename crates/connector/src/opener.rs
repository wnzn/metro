//! Opens a sign-in start link in the person's default browser, and says
//! whether that worked.
//!
//! Only one kind of URL is ever opened: Metro's own one-time loopback start
//! link, `http://127.0.0.1:<port>/auth/start?k=<key>`. It carries no token
//! or code and only redirects to the issuer once the loopback listener
//! checks the key. Anything else is refused, so an arbitrary URL can never be
//! handed to the OS through this path.
//!
//! - Windows: `ShellExecuteW` with the `open` verb (the user's default URL
//!   handler), on a thread of its own with COM initialized, as the Shell
//!   documents for `ShellExecute`. Its return code is checked: a value of 32
//!   or less is a failure and is reported. (`explorer.exe <url>` gives no
//!   usable result, so a failure was silent.)
//! - macOS: `/usr/bin/open`; Linux and other Unix: `xdg-open` by absolute
//!   path. A non-zero exit is a failure.
//!
//! The error text says what to do next and never contains the URL.

/// True for Metro's loopback start link and nothing else.
pub fn is_start_link(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let key_ok = {
        let mut pairs = u.query_pairs();
        match (pairs.next(), pairs.next()) {
            (Some((k, v)), None) => {
                k == "k"
                    && (16..=128).contains(&v.len())
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            }
            _ => false,
        }
    };
    u.scheme() == "http"
        && u.host_str() == Some("127.0.0.1")
        && u.port().is_some_and(|p| p != 0)
        && u.username().is_empty()
        && u.password().is_none()
        && u.path() == "/auth/start"
        && u.fragment().is_none()
        && key_ok
}

/// Opens `url` (a start link, see [`is_start_link`]) in the default browser.
/// `Err` holds a short message for the person, without the URL.
pub fn open_start_link(url: &str) -> Result<(), String> {
    if !is_start_link(url) {
        return Err("Metro only opens its own sign-in link.".into());
    }
    platform::open(url)
}

#[cfg(windows)]
mod platform {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    };
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn open(url: &str) -> Result<(), String> {
        let url = url.to_owned();
        // Its own thread: COM is initialized there as ShellExecute wants,
        // whatever the caller's thread is doing.
        let worker = std::thread::spawn(move || {
            let file = wide(&url);
            let verb = wide("open");
            // SAFETY: NUL-terminated UTF-16 strings that outlive the call;
            // COM is initialized and uninitialized on this thread only.
            unsafe {
                let hr = CoInitializeEx(
                    std::ptr::null(),
                    (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as _,
                );
                let r = ShellExecuteW(
                    std::ptr::null_mut(),
                    verb.as_ptr(),
                    file.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    SW_SHOWNORMAL,
                ) as isize;
                if hr >= 0 {
                    CoUninitialize();
                }
                r
            }
        });
        let code = worker
            .join()
            .map_err(|_| "Windows couldn't open the default web browser.".to_string())?;
        if code > 32 {
            return Ok(());
        }
        Err(match code {
            // SE_ERR_NOASSOC, SE_ERR_ASSOCINCOMPLETE
            31 | 27 => "Windows has no default web browser set for web links. Choose one in Settings › Apps › Default apps.".into(),
            // SE_ERR_ACCESSDENIED
            5 => "Windows refused to open the default web browser (access denied).".into(),
            n => format!(
                "Windows couldn't open the default web browser (ShellExecute error {n})."
            ),
        })
    }
}

#[cfg(unix)]
mod platform {
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// How long an opener may run before it is taken as having launched.
    const UNIX_WAIT: Duration = Duration::from_secs(15);

    pub fn open(url: &str) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        let candidates = [PathBuf::from("/usr/bin/open")];
        #[cfg(not(target_os = "macos"))]
        let candidates = [
            PathBuf::from("/usr/bin/xdg-open"),
            PathBuf::from("/usr/local/bin/xdg-open"),
            PathBuf::from("/bin/xdg-open"),
        ];
        let Some(bin) = candidates.into_iter().find(|p| p.is_file()) else {
            return Err("This computer has no system browser opener. Set up a default web browser and try again.".into());
        };
        let mut child = Command::new(&bin)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "The browser opener couldn't be started. Check your default web browser and try again.".to_string())?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(format!(
                        "No browser opened (the opener exited with {}). Set a default web browser and try again.",
                        status.code().map_or_else(|| "a signal".into(), |c| c.to_string())
                    ));
                }
                // Still running: openers that stay attached to the browser
                // have launched it.
                Ok(None) if started.elapsed() >= UNIX_WAIT => return Ok(()),
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => return Err("The browser opener didn't report back. Check your default web browser and try again.".into()),
            }
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    pub fn open(_: &str) -> Result<(), String> {
        Err("Metro can't open a browser on this platform.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_loopback_start_link_is_opened() {
        let key = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";
        assert!(is_start_link(&format!(
            "http://127.0.0.1:4321/auth/start?k={key}"
        )));
        for bad in [
            format!("https://127.0.0.1:4321/auth/start?k={key}"),
            format!("http://localhost:4321/auth/start?k={key}"),
            format!("http://127.0.0.1/auth/start?k={key}"),
            format!("http://127.0.0.1:4321/auth/callback?k={key}"),
            format!("http://127.0.0.1:4321/auth/start?k={key}&x=1"),
            format!("http://127.0.0.1:4321/auth/start?k={key}#f"),
            format!("http://u@127.0.0.1:4321/auth/start?k={key}"),
            "http://127.0.0.1:4321/auth/start?k=short".into(),
            "https://auth.openai.com/oauth/authorize?client_id=x".into(),
            "file:///etc/passwd".into(),
            "javascript:alert(1)".into(),
        ] {
            assert!(!is_start_link(&bad), "{bad}");
            assert!(open_start_link(&bad).is_err(), "{bad}");
        }
    }
}
