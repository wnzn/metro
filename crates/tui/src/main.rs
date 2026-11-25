mod api;
mod app;
mod composer;
mod terminal;
mod theme;

use std::{
    fs::OpenOptions,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use clap::{Parser, error::ErrorKind};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::{
    api::Api,
    app::App,
    terminal::{RawMode, Screen},
};

const MAX_TOKEN: usize = 16_384;
const INPUT_TICK: Duration = Duration::from_millis(75);

#[derive(Parser)]
#[command(version, about = "Metro conversations in an interactive terminal")]
struct Args {
    /// Explicit Metro origin (HTTPS or HTTP; plain HTTP outside loopback and Tailscale prints a warning)
    #[arg(long, value_name = "ORIGIN")]
    server: String,
    /// Read a token from a private regular file instead of prompting without echo
    #[arg(long, value_name = "PATH")]
    token_file: Option<PathBuf>,
}

fn token_file(path: &Path) -> Result<String, &'static str> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| "Could not open the token file.")?;
    let metadata = file
        .metadata()
        .map_err(|_| "Could not check the token file.")?;
    if !metadata.is_file() {
        return Err("The token file must be a regular file.");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no arguments and does not touch memory.
        let uid = unsafe { libc::geteuid() };
        if metadata.mode() & 0o077 != 0 || metadata.uid() != uid {
            return Err("The token file must belong to you and be private (chmod 600).");
        }
    }
    let mut token = String::new();
    file.take(MAX_TOKEN as u64 + 1)
        .read_to_string(&mut token)
        .map_err(|_| "Could not read the token file.")?;
    if token.len() > MAX_TOKEN {
        return Err("The token file is too large.");
    }
    Ok(token.trim_end_matches(['\r', '\n']).to_owned())
}

fn prompt_token(interrupted: &AtomicBool) -> io::Result<String> {
    // Disable echo before the prompt is even visible. Polling also makes a
    // process-level SIGINT or SSH disconnect leave this scope and restore tty.
    let _raw = RawMode::enter()?;
    let mut out = io::stdout();
    out.write_all(b"Metro token: ")?;
    out.flush()?;
    let mut token = String::new();
    while !interrupted.load(Ordering::Relaxed) {
        if !event::poll(INPUT_TICK)? {
            continue;
        }
        match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Enter => {
                    out.write_all(b"\r\n")?;
                    out.flush()?;
                    return Ok(token);
                }
                KeyCode::Esc => break,
                KeyCode::Backspace => {
                    token.pop();
                }
                KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    token.clear()
                }
                KeyCode::Char(c)
                    if c.is_ascii_graphic()
                        && !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                {
                    token.push(c)
                }
                _ => {}
            },
            Event::Paste(text) if text.bytes().all(|b| b.is_ascii_graphic()) => {
                token.push_str(&text)
            }
            _ => {}
        }
        if token.len() > MAX_TOKEN {
            return Err(io::Error::other("token too large"));
        }
    }
    out.write_all(b"\r\n")?;
    Err(io::ErrorKind::Interrupted.into())
}

#[cfg(unix)]
fn install_interrupt_handlers(flag: &Arc<AtomicBool>) -> io::Result<()> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};

    for signal in [SIGINT, SIGHUP, SIGTERM] {
        signal_hook::flag::register(signal, Arc::clone(flag))?;
    }
    Ok(())
}

#[cfg(windows)]
fn install_interrupt_handlers(flag: &Arc<AtomicBool>) -> Result<(), ctrlc::Error> {
    let flag = Arc::clone(flag);
    ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed))
}

fn run(args: Args) -> Result<(), &'static str> {
    let origin = api::origin(&args.server)?;
    if let Some(w) = api::plain_http_warning(&origin) {
        eprintln!("metro-tui: warning: {w}");
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("An interactive terminal is required (allocate a PTY when using SSH).");
    }
    if theme::dumb_terminal() {
        return Err(
            "TERM=dumb lacks cursor addressing. Use an interactive terminal with a matching TERM.",
        );
    }
    let interrupted = Arc::new(AtomicBool::new(false));
    install_interrupt_handlers(&interrupted)
        .map_err(|_| "Could not install terminal cleanup signal handling.")?;
    let token = match args.token_file {
        Some(path) => token_file(&path)?,
        None => prompt_token(&interrupted)
            .map_err(|_| "Could not read a hidden token from the terminal.")?,
    };
    let api = Api::new(origin, &token)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|_| "Could not initialize the network runtime.")?;
    let result = interact(api, runtime.handle().clone(), &token, &interrupted);
    runtime.shutdown_timeout(Duration::from_millis(200));
    result
}

fn interact(
    api: Api,
    runtime: tokio::runtime::Handle,
    token: &str,
    interrupted: &AtomicBool,
) -> Result<(), &'static str> {
    const IO_ERROR: &str = "Terminal input or output failed.";
    let mut screen = Screen::enter().map_err(|_| IO_ERROR)?;
    let mut app = App::new(api, runtime);
    let mut redraw = true;
    while !interrupted.load(Ordering::Relaxed) {
        redraw |= app.tick();
        if redraw {
            screen.draw(&mut app, token).map_err(|_| IO_ERROR)?;
            redraw = false;
        }
        if event::poll(INPUT_TICK).map_err(|_| IO_ERROR)? {
            match event::read().map_err(|_| IO_ERROR)? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    if !app.key(
                        key,
                        key.kind == KeyEventKind::Repeat,
                        screen.split_available(),
                    ) {
                        break;
                    }
                    redraw = true;
                }
                Event::Paste(text) => {
                    app.insert(&text);
                    redraw = true;
                }
                Event::Resize(_, _) => redraw = true,
                _ => {}
            }
        }
    }
    if app.fatal {
        Err("Could not connect or sign in to Metro.")
    } else {
        Ok(())
    }
}

fn main() -> std::process::ExitCode {
    let args = match Args::try_parse() {
        Ok(args) => args,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.exit()
        }
        // Do not echo mistaken arguments, which might include credentials.
        Err(_) => {
            eprintln!(
                "metro-tui: Expected --server <ORIGIN> and optional --token-file <PATH>. See --help."
            );
            return std::process::ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("metro-tui: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}
