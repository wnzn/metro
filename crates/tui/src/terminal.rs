use std::io::{self, Stdout, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute, queue,
    style::Print,
    terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    api::Recipient,
    app::{App, Focus, Overlay},
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
            secret,
            cols: cols.saturating_sub(1) as usize,
            rows: rows as usize,
        };
        queue!(frame.out, Hide, Clear(ClearType::All))?;
        let width = frame.cols;
        let height = frame.rows;
        frame.line(0, 0, width, &format!("Metro   |   {}", app.identity))?;
        if width < 18 || height < 7 {
            frame.line(0, 1, width, "Resize · F10 quit")?;
        } else if let Some(overlay) = &app.overlay {
            match overlay {
                Overlay::Help => {
                    frame.line(0, 2, width, "KEYBOARD HELP   ·   Esc to close")?;
                    let mut y = 3;
                    for text in HELP {
                        for line in wrap(text, width).0 {
                            frame.line(0, y, width, &line)?;
                            y += 1;
                        }
                        if y >= height - 1 {
                            break;
                        }
                    }
                    if y >= height - 1 {
                        frame.line(0, height - 1, width, "Enlarge terminal for full help")?;
                    }
                }
                Overlay::Models => {
                    frame.line(
                        0,
                        2,
                        width,
                        "MODEL & THINKING   ·   ↑/↓ Enter   ·   Esc back",
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
                        frame.line(0, 5 + i - start, width, &format!("{marker} {}", row.text))?;
                    }
                    if let Some(row) = rows.get(app.model_selected) {
                        frame.line(0, height - 1, width, row.detail)?;
                    }
                }
            }
        } else if app.fatal {
            for (i, line) in wrap(&app.transcript_status, width).0.iter().enumerate() {
                frame.line(0, 2 + i, width, line)?;
            }
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
                frame.line(0, 2, nav_width, heading)?;
                let nav = app.nav_rows();
                let room = height - 5;
                let start = list_start(nav.len(), app.nav_selected, room);
                for (i, row) in nav.iter().enumerate().skip(start).take(room) {
                    let marker = if i == app.nav_selected { ">" } else { " " };
                    frame.line(
                        0,
                        3 + i - start,
                        nav_width,
                        &format!("{marker} {}", row.text),
                    )?;
                }
                frame.line(0, height - 2, nav_width, &app.nav_status)?;
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
        let _ = execute!(self.out, Show, LeaveAlternateScreen);
    }
}

struct Frame<'a> {
    out: &'a mut Stdout,
    secret: &'a str,
    cols: usize,
    rows: usize,
}

impl Frame<'_> {
    fn line(&mut self, x: usize, y: usize, width: usize, text: &str) -> io::Result<()> {
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
        queue!(self.out, MoveTo(x as u16, y as u16), Print(&text[..end]))
    }

    fn conversation(
        &mut self,
        app: &mut App,
        x: usize,
        width: usize,
        height: usize,
    ) -> io::Result<()> {
        self.line(x, 2, width, &app.title)?;
        let caption = if app.catalog_status.is_empty() {
            app.model_caption()
        } else {
            app.catalog_status.clone()
        };
        if height >= 9 {
            self.line(x, 3, width, &format!("Model: {caption}"))?;
        }

        let draft_height = if height >= 15 { 3 } else { 1 };
        let draft_top = height - 2 - draft_height;
        let status_y = draft_top - 1;
        let transcript_top = if height >= 9 { 4 } else { 3 };
        let room = status_y.saturating_sub(transcript_top);
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
                lines.extend(wrap(&single_line(&title, self.secret), width).0);
                let text = redact(&entry.text, self.secret);
                let limit = 16 * 1024;
                lines.extend(
                    wrap(
                        &text[..text.floor_char_boundary(text.len().min(limit))],
                        width,
                    )
                    .0,
                );
                if text.len() > limit {
                    lines.push("[entry display truncated]".into());
                }
                lines.push(String::new());
            }
            if lines.is_empty() && app.transcript_status.is_empty() {
                lines.push("No messages yet.".into());
            }
            app.scroll = app.scroll.min(lines.len().saturating_sub(room));
            let end = lines.len().saturating_sub(app.scroll);
            for (i, line) in lines[end.saturating_sub(room)..end].iter().enumerate() {
                self.line(x, transcript_top + i, width, line)?;
            }
        }

        let draft = &app.current().draft;
        let status = if app.sending_here() {
            "Sending…".to_owned()
        } else if let Some(note) = &draft.notice {
            note.clone()
        } else if !app.transcript_status.is_empty() {
            app.transcript_status.clone()
        } else if app.scroll > 0 {
            format!("Scrolled up {} lines · PgDn for latest", app.scroll)
        } else {
            String::new()
        };
        let status_width = wrap(&single_line(&status, self.secret), width).0;
        let status_lines = status_width.len().min(if room > 1 { 2 } else { 1 });
        for (i, line) in status_width.iter().take(status_lines).enumerate() {
            self.line(x, status_y + i + 1 - status_lines, width, line)?;
        }

        let draft_width = width.saturating_sub(3).max(1);
        let text = redact(&draft.text, self.secret);
        let (lines, cursor_row, cursor_col) = wrap_at(&text, draft_width, Some(draft.caret));
        let top = cursor_row
            .saturating_sub(draft_height - 1)
            .min(lines.len().saturating_sub(draft_height));
        self.line(
            x,
            draft_top,
            2,
            if app.focus == Focus::Draft { ">" } else { "·" },
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
        self.line(
            x,
            height - 2,
            width,
            &format!("Enter {enter} · F2 Ask · F3 {alternate}"),
        )?;
        self.line(
            x,
            height - 1,
            width,
            "Tab Browse · F4 Model · F5 Refresh · F1 Help · F10 Quit",
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
