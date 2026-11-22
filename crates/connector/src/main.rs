use std::process::ExitCode;

fn main() -> ExitCode {
    // Files we create are private even if a code path forgets to set a mode.
    #[cfg(unix)]
    // SAFETY: umask has no preconditions.
    unsafe {
        libc::umask(0o077);
    }
    // SAFETY: first thing in main, no other thread yet.
    unsafe {
        metro_connector::http::exempt_loopback_from_proxy();
    }
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("metro-connector: cannot start: {e}");
            return ExitCode::from(1);
        }
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = rt.block_on(metro_connector::cli::main_with(args));
    ExitCode::from(code.clamp(0, 255) as u8)
}
