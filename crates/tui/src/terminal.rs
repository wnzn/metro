use std::io::{self, Stdout, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute, queue,
    style::{Attribute, Print, SetAttribute},
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    api::Recipient,
    app::{App, Focus, Overlay},
    theme::{Mode, Tone},
};

const HELP: &[&str] = &[
    "F1 / Esc    Close help                 F10 / Ctrl-C    Quit",
    "Tab         Switch between conversations and draft",
    "↑ / ↓       Browse Spaces, rooms, or models",
    "Enter       Expand a Space, open a conversation, or choose a model",
    "Enter       In the draft: use the action named below it",
    "F2          Ask agent explicitly (adds @agent to any recipients)",
    "F3          Send / Save note; typed mentions and server routing still apply",
    "F4          Pick a model and an advertised thinking level",
    "F5          Refresh Spaces, rooms, transcript, and available models",
    "F6          Insert a newline in the draft",
    "PgUp/PgDn   Scroll transcript when the draft has focus",
    "← / →       Move the caret; Home / End jumps to draft boundaries",
    "",
    "Room recipients: type standalone @agent or @member-id.",
    "People-only posts never start the agent. Unaddressed Send follows your Metro settings.",
    "Drafts stay in this process, per conversation, and are not saved to disk.",
];

pub struct Screen {
    out: Stdout,
    paste: bool,
    mode: Mode,
    _raw: RawMode,
}

pub struct RawMode;

impl RawMode {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

impl Screen {
    pub fn enter() -> io::Result<Self> {
        let raw = RawMode::enter()?;
        let mut screen = Self {
            out: io::stdout(),
            paste: false,
            mode: Mode::detect(),
            _raw: raw,
        };
        execute!(screen.out, EnterAlternateScreen, Hide)?;
        // Legacy Windows consoles don't support this mode. The normal
        // keyboard UI still works there.
        screen.paste = execute!(screen.out, EnableBracketedPaste).is_ok();
        Ok(screen)
    }

    pub fn draw(&mut self, app: &mut App, secret: &str) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        let mut frame = Frame {
            out: &mut self.out,
            mode: self.mode,
            secret,
            cols: cols.saturating_sub(1) as usize,
            rows: rows as usize,
        };
        queue!(frame.out, Hide, Clear(ClearType::All))?;
        let width = frame.cols;
        let height = frame.rows;
        frame.header(app)?;
        if width < 18 || height < 7 {
            frame.line(0, 1, width, "Resize · F10 quit")?;
        } else if let Some(overlay) = &app.overlay {
            match overlay {
                Overlay::Help => {
                    frame.paint(
                        0,
                        2,
                        width,
                        "KEYBOARD HELP   ·   Esc to close",
                        Tone::Strong,
                    )?;
                    let mut y = 3;
                    'help: for text in HELP {
                        for line in wrap(text, width).0 {
                            if y >= height - 1 {
                                frame.line(0, height - 1, width, "Enlarge for full help")?;
                                break 'help;
                            }
                            frame.line(0, y, width, &line)?;
                            y += 1;
                        }
                    }
                }
                Overlay::Models => {
                    frame.paint(
                        0,
                        2,
                        width,
                        "MODEL & THINKING   ·   ↑/↓ Enter   ·   Esc back",
                        Tone::Strong,
                    )?;
                    let caption = if app.catalog_status.is_empty() {
                        app.model_caption()
                    } else {
                        app.catalog_status.clone()
                    };
                    frame.line(0, 3, width, &caption)?;
                    let rows = app.model_rows();
                    let room = height - 6;
                    let start = list_start(rows.len(), app.model_selected, room);
                    for (i, row) in rows.iter().enumerate().skip(start).take(room) {
                        let marker = if i == app.model_selected { ">" } else { " " };
                        frame.paint(
                            0,
                            5 + i - start,
                            width,
                            &format!("{marker} {}", row.text),
                            if i == app.model_selected {
                                Tone::Selected
                            } else {
                                Tone::Plain
                            },
                        )?;
                    }
                    if let Some(row) = rows.get(app.model_selected) {
                        frame.line(0, height - 1, width, row.detail)?;
                    }
                }
            }
        } else if app.fatal {
            frame.status(
                0,
                2,
                width,
                height - 3,
                (Tone::Error, "Error"),
                &app.transcript_status,
            )?;
            frame.line(0, height - 1, width, "F10 / Ctrl-C quit")?;
        } else {
            let browser = app.focus == Focus::Browser;
            let split = width >= 78;
            let nav_width = if split { (width / 3).min(32) } else { width };
            if split || browser {
                let heading = if browser {
                    "CONVERSATIONS  <"
                } else {
                    "CONVERSATIONS"
                };
                frame.paint(0, 2, nav_width, heading, Tone::Strong)?;
                let nav = app.nav_rows();
                let room = height - 5;
                let start = list_start(nav.len(), app.nav_selected, room);
                for (i, row) in nav.iter().enumerate().skip(start).take(room) {
                    let marker = if i == app.nav_selected { ">" } else { " " };
                    frame.paint(
                        0,
                        3 + i - start,
                        nav_width,
                        &format!("{marker} {}", row.text),
                        if i == app.nav_selected {
                            Tone::Selected
                        } else {
                            Tone::Plain
                        },
                    )?;
                }
                if !app.nav_status.is_empty() {
                    let loading = app.nav_status.starts_with("Loading");
                    frame.status(
                        0,
                        height - 2,
                        nav_width,
                        1,
                        if loading {
                            (Tone::Info, "Info")
                        } else {
                            (Tone::Error, "Error")
                        },
                        &app.nav_status,
                    )?;
                }
            }
            if !split && browser {
                frame.line(0, height - 1, width, "Enter open · Tab draft · F1 help")?;
            } else {
                let x = if split { nav_width + 2 } else { 0 };
                frame.conversation(app, x, width - x, height)?;
            }
        }
        frame.out.flush()
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        if self.paste {
            let _ = execute!(self.out, DisableBracketedPaste);
        }
        let _ = execute!(
            self.out,
            SetAttribute(Attribute::Reset),
            Show,
            LeaveAlternateScreen
        );
    }
}

struct Frame<'a> {
    out: &'a mut Stdout,
    mode: Mode,
    secret: &'a str,
    cols: usize,
    rows: usize,
}

impl Frame<'_> {
    fn line(&mut self, x: usize, y: usize, width: usize, text: &str) -> io::Result<()> {
        self.paint(x, y, width, text, Tone::Plain)
    }

    fn paint(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        text: &str,
        tone: Tone,
    ) -> io::Result<()> {
        if y >= self.rows || x >= self.cols || width == 0 {
            return Ok(());
        }
        let text = single_line(text, self.secret);
        let mut used = 0;
        let end = text
            .grapheme_indices(true)
            .find_map(|(i, g)| {
                used += g.width();
                (used > width).then_some(i)
            })
            .unwrap_or(text.len());
        queue!(self.out, MoveTo(x as u16, y as u16))?;
        self.mode.begin(self.out, tone)?;
        queue!(self.out, Print(&text[..end]))?;
        self.mode.end(self.out, tone)
    }

    fn header(&mut self, app: &App) -> io::Result<()> {
        self.paint(0, 0, self.cols, " metro ", Tone::Brand)?;
        if self.cols <= 7 {
            return Ok(());
        }
        let (label, tone) = if app.fatal {
            ("Offline", Tone::Error)
        } else if app.is_signed_in() {
            ("Signed in", Tone::Success)
        } else {
            ("Connecting", Tone::Info)
        };
        let badge = format!(" [{label}] ");
        let body_width = self.cols - 7;
        // On narrow screens give the identity more room than the badge.
        let width = if body_width >= badge.len() + 12 {
            body_width - badge.len()
        } else {
            body_width
        };
        self.paint(7, 0, width, &" ".repeat(width), Tone::Rail)?;
        self.paint(7, 0, width, &format!(" | {}", app.identity), Tone::Rail)?;
        if width < body_width {
            self.paint(7 + width, 0, badge.len(), &badge, tone)?;
        }
        Ok(())
    }

    fn status(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        max_lines: usize,
        (tone, label): (Tone, &str),
        detail: &str,
    ) -> io::Result<()> {
        let chip = format!("[{label}]");
        let indent = chip.len() + 1;
        self.paint(x, y, width, &chip, tone)?;
        let available = width.saturating_sub(indent);
        if available > 0 {
            let safe = single_line(detail, self.secret);
            for (i, line) in wrap(&safe, available).0.iter().take(max_lines).enumerate() {
                self.line(x + indent, y + i, available, line)?;
            }
        }
        Ok(())
    }

    fn conversation(
        &mut self,
        app: &mut App,
        x: usize,
        width: usize,
        height: usize,
    ) -> io::Result<()> {
        self.paint(x, 2, width, &app.title, Tone::Strong)?;
        let caption = if app.catalog_status.is_empty() {
            app.model_caption()
        } else {
            app.catalog_status.clone()
        };
        if height >= 9 {
            if app.current().blocked {
                self.status(x, 3, width, 1, (Tone::Warning, "Warning"), &caption)?;
            } else {
                self.line(x, 3, width, &format!("Model: {caption}"))?;
            }
        }

        let draft_height = if height >= 15 { 3 } else { 1 };
        let draft_top = height - 2 - draft_height;
        let status_y = draft_top - 1;
        let transcript_top = if height >= 9 { 4 } else { 3 };
        let status = if app.sending_here() {
            Some(((Tone::Info, "Info"), "Sending…"))
        } else if let Some(note) = &app.current().draft.notice {
            Some(((Tone::Warning, "Warning"), note.as_str()))
        } else if !app.transcript_status.is_empty() {
            let loading = app.transcript_status.starts_with("Loading") || !app.is_signed_in();
            let badge = if loading {
                (Tone::Info, "Info")
            } else {
                (Tone::Error, "Error")
            };
            Some((badge, app.transcript_status.as_str()))
        } else {
            None
        };
        let spare = status_y.saturating_sub(transcript_top);
        let multiline = spare > 1
            && status.is_some_and(|((_, label), detail)| {
                detail.width() > width.saturating_sub(label.len() + 3)
            });
        let room = spare - usize::from(multiline);
        let status_drawn = status.is_some();
        // Reserve the status rows first, so a wrapped notice never paints
        // over part of a transcript line.
        if let Some((badge, detail)) = status {
            self.status(
                x,
                status_y - usize::from(multiline),
                width,
                1 + usize::from(multiline),
                badge,
                detail,
            )?;
        }
        let mut lines = Vec::new();
        if room > 0 {
            for entry in &app.entries {
                let by = match &entry.actor_id {
                    Some(id) if id != &entry.actor => format!("{} ({id})", entry.actor),
                    _ => entry.actor.clone(),
                };
                let to = entry
                    .mentions
                    .iter()
                    .filter_map(|m| match m {
                        Recipient::Agent => Some("@agent".to_owned()),
                        Recipient::User { id } => Some(format!("@{id}")),
                        Recipient::Other => None,
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                let title = if !to.is_empty() {
                    format!("{by} → {to}")
                } else if entry.kind == "message" {
                    format!("{by} → everyone")
                } else {
                    by
                };
                let (title, tone) = match entry.kind.as_str() {
                    "agent" => (format!("[AI] {title}"), Tone::Ai),
                    "error" => (format!("[Error] {title}"), Tone::Error),
                    _ => (title, Tone::Strong),
                };
                lines.extend(
                    wrap(&single_line(&title, self.secret), width)
                        .0
                        .into_iter()
                        .enumerate()
                        .map(|(i, line)| (if i == 0 { tone } else { Tone::Strong }, line)),
                );
                let text = redact(&entry.text, self.secret);
                let limit = 16 * 1024;
                lines.extend(
                    wrap(
                        &text[..text.floor_char_boundary(text.len().min(limit))],
                        width,
                    )
                    .0
                    .into_iter()
                    .map(|line| (Tone::Plain, line)),
                );
                if text.len() > limit {
                    lines.push((Tone::Strong, "[entry display truncated]".into()));
                }
                lines.push((Tone::Plain, String::new()));
            }
            if lines.is_empty() && app.transcript_status.is_empty() {
                lines.push((Tone::Plain, "No messages yet.".into()));
            }
            app.scroll = app.scroll.min(lines.len().saturating_sub(room));
            let end = lines.len().saturating_sub(app.scroll);
            for (i, (tone, line)) in lines[end.saturating_sub(room)..end].iter().enumerate() {
                // Keep the chip short; the rest of the header reads in the user's colors.
                let chip = match tone {
                    Tone::Ai => Some("[AI]"),
                    Tone::Error => Some("[Error]"),
                    _ => None,
                };
                if let Some(chip) = chip {
                    self.paint(x, transcript_top + i, width, chip, *tone)?;
                    self.line(
                        x + chip.len() + 1,
                        transcript_top + i,
                        width.saturating_sub(chip.len() + 1),
                        line.strip_prefix(chip).unwrap_or(line).trim_start(),
                    )?;
                } else {
                    self.paint(x, transcript_top + i, width, line, *tone)?;
                }
            }
        }

        if !status_drawn && app.scroll > 0 {
            self.status(
                x,
                status_y,
                width,
                1,
                (Tone::Info, "Info"),
                &format!("Scrolled up {} lines · PgDn for latest", app.scroll),
            )?;
        }

        let draft = &app.current().draft;
        let draft_width = width.saturating_sub(3).max(1);
        let text = redact(&draft.text, self.secret);
        let (lines, cursor_row, cursor_col) = wrap_at(&text, draft_width, Some(draft.caret));
        let top = cursor_row
            .saturating_sub(draft_height - 1)
            .min(lines.len().saturating_sub(draft_height));
        let focused = app.focus == Focus::Draft;
        self.paint(
            x,
            draft_top,
            2,
            if focused { ">" } else { "·" },
            if focused { Tone::Selected } else { Tone::Plain },
        )?;
        for (i, line) in lines.iter().skip(top).take(draft_height).enumerate() {
            self.line(x + 2, draft_top + i, width - 2, line)?;
        }
        let enter = if app.enter_asks() {
            "Ask agent"
        } else {
            "Send"
        };
        let alternate = if app.save_note_label() {
            "Save note"
        } else {
            "Send"
        };
        self.paint(
            x,
            height - 2,
            width,
            &format!("Enter {enter} · F2 Ask · F3 {alternate}"),
            Tone::Strong,
        )?;
        self.line(
            x,
            height - 1,
            width,
            if width >= 52 {
                "Tab Browse · F1 Help · F4 Model · F5 Refresh · F10 Quit"
            } else if width >= 22 {
                "Tab Browse · F1 Help"
            } else {
                "Tab · F1 Help"
            },
        )?;
        if app.focus == Focus::Draft && !app.sending_here() {
            queue!(
                self.out,
                MoveTo(
                    (x + 2 + cursor_col) as u16,
                    (draft_top + cursor_row - top) as u16
                ),
                Show
            )?;
        }
        Ok(())
    }
}

fn list_start(len: usize, selected: usize, room: usize) -> usize {
    selected
        .saturating_sub(room / 2)
        .min(len.saturating_sub(room))
}

// Keep byte offsets valid for the draft's caret. All accepted token bytes
// are ASCII. Redact before clipping or splitting a long string into lines.
fn redact(text: &str, secret: &str) -> String {
    text.replace(secret, &"*".repeat(secret.len()))
}

fn printable(c: char) -> Option<char> {
    match c {
        '\t' => Some(' '),
        '\u{2028}' | '\u{2029}' => Some(' '),
        '\u{061c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}' => None,
        c if !c.is_control() => Some(c),
        _ => None,
    }
}

fn single_line(text: &str, secret: &str) -> String {
    redact(text, secret)
        .chars()
        .filter_map(|c| {
            if matches!(c, '\r' | '\n') {
                Some(' ')
            } else {
                printable(c)
            }
        })
        .collect()
}

fn wrap(text: &str, width: usize) -> (Vec<String>, usize, usize) {
    wrap_at(text, width, None)
}

fn wrap_at(text: &str, width: usize, at: Option<usize>) -> (Vec<String>, usize, usize) {
    let mut lines = vec![String::new()];
    let (mut y, mut x) = (0, 0);
    let mut cursor = None;
    for (i, g) in text.grapheme_indices(true) {
        if matches!(g, "\r\n" | "\n") {
            if at == Some(i) {
                cursor = Some((y, x));
            }
            lines.push(String::new());
            y += 1;
            x = 0;
        } else {
            let safe: String = g.chars().filter_map(printable).collect();
            let cells = safe.width();
            if x > 0 && x + cells > width {
                lines.push(String::new());
                y += 1;
                x = 0;
            }
            if at.is_some_and(|at| (i..i + g.len()).contains(&at)) {
                cursor = Some((y, x));
            }
            lines[y].push_str(&safe);
            x += cells;
        }
    }
    let (y, x) = cursor.unwrap_or((y, x));
    (lines, y, x.min(width))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_text_loses_terminal_codes_and_tokens_before_clipping() {
        let token = "abc012345";
        let text = redact("中 \x1b]52;c;abc012345\x07 \u{009b}2J\r\n🙂", token);
        let lines = wrap(&text, 5).0;
        assert!(lines.iter().all(|line| !line.chars().any(char::is_control)));
        assert!(!lines.concat().contains(token));
        assert_eq!(lines.last().unwrap(), "🙂");
        assert_eq!(wrap_at("ab中d", 3, Some(5)).1, 1);
    }
}
