use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::mpsc::{Receiver, Sender, channel},
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::{
    api::{Api, CatalogRow, Choice, Entry, Failure, Named, Place, Post},
    composer::{Catalog, Draft, address},
};

const REFRESH: Duration = Duration::from_secs(2);
const BACKFILL: Duration = Duration::from_millis(250);
const HISTORY: usize = 300;

#[derive(Clone, PartialEq, Eq)]
pub enum Nav {
    Chat,
    Space(String),
    Room(Place),
}

pub struct NavRow {
    pub id: Nav,
    pub text: String,
}

pub struct ModelRow {
    pub choice: Option<Choice>,
    pub text: String,
    pub detail: &'static str,
}

#[derive(Default)]
pub struct Conversation {
    pub draft: Draft,
    pub pin: Option<Choice>,
    pub blocked: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Browser,
    Draft,
}

pub enum Overlay {
    Help,
    Models,
}

enum Answer {
    Me(Result<String, Failure>),
    Providers(Result<Vec<Named>, Failure>),
    Spaces(Result<Vec<Named>, Failure>),
    Rooms(String, Result<Vec<Named>, Failure>),
    Events(Result<Vec<Entry>, Failure>),
    Catalog(Result<Vec<CatalogRow>, Failure>),
    Post(Place, Result<(), Failure>),
}

pub struct App {
    api: Api,
    runtime: Handle,
    send: Sender<(u64, Answer)>,
    recv: Receiver<(u64, Answer)>,
    generation: u64,
    nav_generation: u64,
    catalog_generation: u64,
    events_job: Option<AbortHandle>,
    catalog_job: Option<AbortHandle>,
    spaces_job: Option<AbortHandle>,
    rooms_job: Option<AbortHandle>,
    providers: HashMap<String, String>,
    raw_catalog: Option<Vec<CatalogRow>>,
    pub catalog: Option<Catalog>,
    pub catalog_status: String,
    spaces: Vec<Named>,
    rooms: Vec<Named>,
    expanded: Option<String>,
    pub nav_selected: usize,
    pub nav_status: String,
    pub model_selected: usize,
    pub focus: Focus,
    pub overlay: Option<Overlay>,
    pub identity: String,
    signed_in: bool,
    pub fatal: bool,
    pub place: Place,
    pub title: String,
    pub conversations: HashMap<Place, Conversation>,
    pub entries: VecDeque<Entry>,
    after: i64,
    next_refresh: Instant,
    pub transcript_status: String,
    pub scroll: usize,
    readable: bool,
    denied: bool,
    pending: Option<Place>,
}

impl App {
    pub fn new(api: Api, runtime: Handle) -> Self {
        let (send, recv) = channel();
        let app = Self {
            api,
            runtime,
            send,
            recv,
            generation: 0,
            nav_generation: 0,
            catalog_generation: 0,
            events_job: None,
            catalog_job: None,
            spaces_job: None,
            rooms_job: None,
            providers: HashMap::new(),
            raw_catalog: None,
            catalog: None,
            catalog_status: String::new(),
            spaces: vec![],
            rooms: vec![],
            expanded: None,
            nav_selected: 0,
            nav_status: String::new(),
            model_selected: 0,
            focus: Focus::Draft,
            overlay: None,
            identity: "Connecting…".into(),
            signed_in: false,
            fatal: false,
            place: Place::Chat,
            title: "Personal Chat".into(),
            conversations: HashMap::from([(Place::Chat, Conversation::default())]),
            entries: VecDeque::new(),
            after: 0,
            next_refresh: Instant::now(),
            transcript_status: "Connecting…".into(),
            scroll: 0,
            readable: false,
            denied: false,
            pending: None,
        };
        let api = app.api.clone();
        app.spawn(0, async move { Answer::Me(api.me().await.map(|m| m.id)) });
        app
    }

    fn spawn(&self, tag: u64, task: impl Future<Output = Answer> + Send + 'static) -> AbortHandle {
        let tx = self.send.clone();
        self.runtime
            .spawn(async move {
                let _ = tx.send((tag, task.await));
            })
            .abort_handle()
    }

    pub fn current(&self) -> &Conversation {
        &self.conversations[&self.place]
    }
    fn current_mut(&mut self) -> &mut Conversation {
        self.conversations.get_mut(&self.place).unwrap()
    }
    pub fn sending_here(&self) -> bool {
        self.pending.as_ref() == Some(&self.place)
    }

    fn load_events(&mut self) {
        let api = self.api.clone();
        let place = self.place.clone();
        let after = self.after;
        self.events_job = Some(self.spawn(self.generation, async move {
            Answer::Events(api.events(&place, after).await)
        }));
    }

    fn load_catalog(&mut self) {
        if let Some(job) = self.catalog_job.take() {
            job.abort();
        }
        self.catalog_generation += 1;
        self.raw_catalog = None;
        self.catalog = None;
        self.model_selected = 0;
        self.catalog_status = "Loading available models…".into();
        let api = self.api.clone();
        let place = self.place.clone();
        self.catalog_job = Some(self.spawn(self.catalog_generation, async move {
            Answer::Catalog(api.catalog(&place).await)
        }));
    }

    fn load_nav(&mut self) {
        if self.spaces_job.is_none() {
            let api = self.api.clone();
            self.spaces_job =
                Some(self.spawn(0, async move { Answer::Spaces(api.spaces().await) }));
        }
        if self.rooms_job.is_none()
            && let Some(space) = self.expanded.clone()
        {
            let api = self.api.clone();
            self.rooms_job = Some(self.spawn(self.nav_generation, async move {
                let result = api.rooms(&space).await;
                Answer::Rooms(space, result)
            }));
        }
    }

    fn refresh(&mut self) {
        if !self.signed_in {
            return;
        }
        self.load_nav();
        if self.catalog_job.is_none() {
            self.load_catalog();
        }
        self.denied = false;
        self.next_refresh = Instant::now();
    }

    fn switch(&mut self, place: Place, title: String) {
        self.focus = Focus::Draft;
        if place == self.place {
            return;
        }
        if let Some(job) = self.events_job.take() {
            job.abort();
        }
        self.generation += 1;
        self.entries.clear();
        self.after = 0;
        self.scroll = 0;
        self.readable = false;
        self.denied = false;
        self.transcript_status = "Loading transcript…".into();
        self.outstanding_refresh();
        self.conversations.entry(place.clone()).or_default();
        self.place = place;
        self.title = title;
        self.load_catalog();
    }

    fn outstanding_refresh(&mut self) {
        self.next_refresh = Instant::now();
    }

    pub fn tick(&mut self) -> bool {
        let mut changed = false;
        while let Ok((tag, answer)) = self.recv.try_recv() {
            changed |= self.apply(tag, answer);
        }
        if self.signed_in
            && !self.fatal
            && !self.denied
            && self.events_job.is_none()
            && Instant::now() >= self.next_refresh
        {
            self.load_events();
        }
        changed
    }

    fn apply(&mut self, tag: u64, answer: Answer) -> bool {
        match answer {
            Answer::Me(Ok(id)) => {
                self.identity = id;
                self.signed_in = true;
                self.transcript_status = "Loading transcript…".into();
                let api = self.api.clone();
                self.spawn(0, async move { Answer::Providers(api.providers().await) });
                self.refresh();
            }
            Answer::Me(Err(err)) => {
                self.identity = "Not connected".into();
                self.transcript_status = err.to_string();
                self.fatal = true;
            }
            Answer::Providers(Ok(names)) => {
                self.providers = names.into_iter().map(|n| (n.id, n.name)).collect();
                self.rebuild_catalog();
            }
            Answer::Providers(Err(_)) => return false,
            Answer::Spaces(result) => {
                self.spaces_job = None;
                match result {
                    Ok(spaces) => {
                        let selected = self.nav_rows().get(self.nav_selected).map(|r| r.id.clone());
                        self.spaces = spaces;
                        if self
                            .expanded
                            .as_ref()
                            .is_some_and(|id| !self.spaces.iter().any(|s| &s.id == id))
                        {
                            self.expanded = None;
                            self.rooms.clear();
                            if let Some(job) = self.rooms_job.take() {
                                job.abort();
                            }
                            self.nav_generation += 1;
                        }
                        self.reselect_nav(selected);
                        if self.rooms_job.is_none() {
                            self.nav_status.clear();
                        }
                    }
                    Err(err) => self.nav_status = err.to_string(),
                }
            }
            Answer::Rooms(space, result)
                if tag == self.nav_generation && self.expanded.as_ref() == Some(&space) =>
            {
                self.rooms_job = None;
                let selected = self.nav_rows().get(self.nav_selected).map(|r| r.id.clone());
                match result {
                    Ok(rooms) => {
                        self.rooms = rooms;
                        self.nav_status.clear();
                    }
                    Err(err) => {
                        self.rooms.clear();
                        self.nav_status = err.to_string();
                    }
                }
                self.reselect_nav(selected);
            }
            Answer::Events(result) if tag == self.generation => {
                self.events_job = None;
                self.next_refresh = Instant::now() + REFRESH;
                match result {
                    Ok(events) => {
                        let was_readable = self.readable;
                        self.readable = true;
                        let had_status = !self.transcript_status.is_empty();
                        self.transcript_status.clear();
                        let mut advanced = false;
                        for entry in events {
                            if entry.id <= self.after {
                                continue;
                            }
                            advanced = true;
                            self.after = entry.id;
                            // History keeps messages and replies plus visible agent/run errors;
                            // reaction and tooling chatter are not conversation.
                            if matches!(
                                entry.kind.as_str(),
                                "message" | "agent" | "demo" | "error" | "thread" | "status"
                            ) {
                                self.entries.push_back(entry);
                                if self.entries.len() > HISTORY {
                                    self.entries.pop_front();
                                }
                            }
                        }
                        if advanced {
                            self.next_refresh = Instant::now() + BACKFILL;
                        }
                        return advanced || had_status || !was_readable;
                    }
                    Err(err) => {
                        if matches!(err, Failure::Http(401 | 403 | 404)) {
                            self.entries.clear();
                            self.readable = false;
                            self.denied = true;
                            self.after = 0;
                            self.scroll = 0;
                        }
                        self.transcript_status = err.to_string();
                    }
                }
            }
            Answer::Catalog(result) if tag == self.catalog_generation => {
                self.catalog_job = None;
                match result {
                    Ok(rows) => {
                        self.raw_catalog = Some(rows);
                        self.catalog_status.clear();
                        self.rebuild_catalog();
                        if self
                            .current()
                            .pin
                            .as_ref()
                            .is_some_and(|p| !self.catalog.as_ref().unwrap().accepts(p))
                        {
                            self.current_mut().blocked = true;
                        }
                    }
                    Err(err) => self.catalog_status = err.to_string(),
                }
            }
            Answer::Post(place, result) => {
                self.pending = None;
                let here = self.place == place;
                let conversation = self.conversations.get_mut(&place).unwrap();
                match result {
                    Ok(()) => {
                        conversation.draft = Draft::default();
                        if here {
                            self.scroll = 0;
                            self.outstanding_refresh();
                        }
                    }
                    Err(err) => {
                        let hint = if matches!(err, Failure::Network | Failure::Http(500..=599)) {
                            " Check the transcript before sending again; delivery is uncertain."
                        } else {
                            ""
                        };
                        conversation.draft.notice = Some(format!("{err} Draft kept.{hint}"));
                        if err == Failure::Reselect {
                            conversation.blocked = true;
                            if here {
                                self.load_catalog();
                            }
                        }
                        if here {
                            self.outstanding_refresh();
                        }
                    }
                }
            }
            _ => return false,
        }
        true
    }

    fn rebuild_catalog(&mut self) {
        if let Some(rows) = &self.raw_catalog {
            let previous = self
                .model_rows()
                .get(self.model_selected)
                .and_then(|row| row.choice.clone());
            self.catalog = Some(Catalog::from_rows(
                rows.clone(),
                self.place.space().is_some(),
                &self.providers,
            ));
            self.model_selected = self
                .model_rows()
                .iter()
                .position(|row| row.choice == previous)
                .unwrap_or(0);
        }
    }

    pub fn nav_rows(&self) -> Vec<NavRow> {
        let mut rows = vec![NavRow {
            id: Nav::Chat,
            text: "Personal Chat".into(),
        }];
        for space in &self.spaces {
            let expanded = self.expanded.as_ref() == Some(&space.id);
            rows.push(NavRow {
                id: Nav::Space(space.id.clone()),
                text: format!("{} {}", if expanded { "−" } else { "+" }, space.name),
            });
            if expanded {
                for room in &self.rooms {
                    rows.push(NavRow {
                        id: Nav::Room(Place::Room {
                            space: space.id.clone(),
                            room: room.id.clone(),
                        }),
                        text: format!("    # {}", room.name),
                    });
                }
            }
        }
        rows
    }

    fn reselect_nav(&mut self, id: Option<Nav>) {
        self.nav_selected = self
            .nav_rows()
            .iter()
            .position(|row| Some(&row.id) == id.as_ref())
            .unwrap_or(0);
    }

    fn open_nav(&mut self) {
        if !self.signed_in {
            return;
        }
        let rows = self.nav_rows();
        let Some(row) = rows.get(self.nav_selected) else {
            return;
        };
        match &row.id {
            Nav::Chat => self.switch(Place::Chat, row.text.clone()),
            Nav::Room(Place::Room { space, room }) => {
                let room_name = self
                    .rooms
                    .iter()
                    .find(|r| &r.id == room)
                    .map_or(room, |r| &r.name);
                let space_name = self
                    .spaces
                    .iter()
                    .find(|s| &s.id == space)
                    .map_or(space, |s| &s.name);
                self.switch(
                    Place::Room {
                        space: space.clone(),
                        room: room.clone(),
                    },
                    format!("{space_name} / {room_name}"),
                );
            }
            Nav::Room(Place::Chat) => unreachable!(),
            Nav::Space(space) => {
                if let Some(job) = self.rooms_job.take() {
                    job.abort();
                }
                self.nav_generation += 1;
                self.rooms.clear();
                self.expanded = if self.expanded.as_ref() == Some(space) {
                    None
                } else {
                    Some(space.clone())
                };
                self.nav_status = if self.expanded.is_some() {
                    "Loading rooms…".into()
                } else {
                    String::new()
                };
                self.load_nav();
            }
        }
    }

    pub fn model_rows(&self) -> Vec<ModelRow> {
        let mut rows = vec![ModelRow {
            choice: None,
            text: "Default / routing (no override)".into(),
            detail: "",
        }];
        if let Some(catalog) = &self.catalog {
            let mut provider = None;
            for target in &catalog.targets {
                let group = if provider == Some(&target.provider) {
                    String::new()
                } else {
                    provider = Some(&target.provider);
                    format!("{}  /  ", target.provider)
                };
                rows.push(ModelRow {
                    choice: Some(target.choice.clone()),
                    text: format!("{group}{} · {}", target.model, target.connection),
                    detail: target.detail,
                });
                for effort in &target.efforts {
                    let mut choice = target.choice.clone();
                    choice.reasoning_effort = Some(effort.clone());
                    rows.push(ModelRow {
                        choice: Some(choice),
                        text: format!("    {} · Thinking: {effort}", target.model),
                        detail: target.detail,
                    });
                }
            }
        }
        rows
    }

    pub fn model_caption(&self) -> String {
        let current = self.current();
        let Some(pin) = &current.pin else {
            return "Default / routing".into();
        };
        let target = self.catalog.as_ref().and_then(|c| {
            c.targets.iter().find(|t| {
                t.choice.provider_connection_id == pin.provider_connection_id
                    && t.choice.model_id == pin.model_id
            })
        });
        match (current.blocked, target) {
            (false, Some(t)) => format!(
                "{} / {} · {}{}{}",
                t.provider,
                t.connection,
                t.model,
                pin.reasoning_effort
                    .as_ref()
                    .map_or(String::new(), |e| format!(" · Thinking: {e}")),
                if t.detail.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", t.detail)
                }
            ),
            _ => "Explicit model not verified or unavailable · F4 to reselect".into(),
        }
    }

    fn known_chat_only(&self) -> bool {
        self.place == Place::Chat
            && !self.current().blocked
            && self.catalog.as_ref().is_some_and(|c| {
                !c.system_one
                    && !c.targets.is_empty()
                    && self.current().pin.as_ref().is_none_or(|p| c.accepts(p))
            })
    }

    pub fn enter_asks(&self) -> bool {
        if self.place != Place::Chat || !self.current().draft.ask_on_enter {
            return false;
        }
        let a = address(self.current().draft.text.trim(), None);
        !a.people || a.agent
    }

    pub fn save_note_label(&self) -> bool {
        self.enter_asks() && !address(self.current().draft.text.trim(), None).agent
    }

    pub fn insert(&mut self, text: &str) {
        if self.overlay.is_some() || self.focus != Focus::Draft || self.sending_here() {
            return;
        }
        let known = self.known_chat_only();
        let text: String = text
            .replace("\r\n", "\n")
            .chars()
            .filter_map(|c| match c {
                '\r' => Some('\n'),
                '\n' | '\t' => Some(c),
                _ if !c.is_control() => Some(c),
                _ => None,
            })
            .collect();
        self.current_mut().draft.insert(&text, known);
    }

    fn submit(&mut self, ask: bool) {
        if self.pending.is_some() {
            self.current_mut().draft.notice = Some("Another message is still being sent.".into());
            return;
        }
        if !self.readable {
            self.current_mut().draft.notice = Some("Transcript is not available. Try F5.".into());
            return;
        }
        let text = self.current().draft.text.trim().to_owned();
        if text.is_empty() {
            return;
        }
        let api = self.api.clone();
        let place = self.place.clone();
        let pin = self.current().pin.clone();
        let blocked = self.current().blocked;
        self.pending = Some(place.clone());
        self.current_mut().draft.notice = None;
        self.spawn(0, async move {
            let result = async {
                // If there is no override, let Metro resolve every textual recipient.
                // If there is one, only attach it to an explicitly agent-directed turn.
                let choice = if let Some(pin) = pin {
                    let directed = if ask {
                        true
                    } else if let Some(space) = place.space() {
                        if address(&text, None).agent {
                            let members = api.members(space).await.map_err(preflight)?;
                            address(&text, Some(&members)).agent
                        } else {
                            false
                        }
                    } else {
                        address(&text, None).agent
                    };
                    if directed {
                        if blocked {
                            return Err(Failure::Reselect);
                        }
                        let rows = api.catalog(&place).await.map_err(preflight)?;
                        if !Catalog::from_rows(rows, place.space().is_some(), &HashMap::new())
                            .accepts(&pin)
                        {
                            return Err(Failure::Reselect);
                        }
                        Some(pin)
                    } else {
                        None
                    }
                } else {
                    None
                };
                api.post(
                    &place,
                    &Post {
                        text: &text,
                        invoke_agent: ask,
                        choice: choice.as_ref(),
                    },
                )
                .await
                .map_err(|err| match err {
                    Failure::Http(403 | 404 | 409 | 422 | 503) if choice.is_some() => {
                        Failure::Reselect
                    }
                    other => other,
                })
            }
            .await;
            Answer::Post(place, result)
        });
    }

    pub fn key(&mut self, key: KeyEvent, repeat: bool) -> bool {
        use KeyCode::*;
        if key.code == Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        if key.code == F(10) {
            return false;
        }
        if repeat && matches!(key.code, F(_) | Enter | Esc | Tab | BackTab) {
            return true;
        }
        if key.code == F(1) {
            self.overlay = if matches!(self.overlay, Some(Overlay::Help)) {
                None
            } else {
                Some(Overlay::Help)
            };
            return true;
        }
        if self.overlay.is_some() {
            match key.code {
                Esc => self.overlay = None,
                F(5) if matches!(self.overlay, Some(Overlay::Models)) => self.refresh(),
                Up | Down | PageUp | PageDown | Home | End
                    if matches!(self.overlay, Some(Overlay::Models)) =>
                {
                    let length = self.model_rows().len();
                    step(&mut self.model_selected, length, key.code);
                }
                Enter if matches!(self.overlay, Some(Overlay::Models)) => {
                    if !self.sending_here()
                        && let Some(row) = self.model_rows().get(self.model_selected)
                    {
                        let current = self.current_mut();
                        current.pin = row.choice.clone();
                        current.blocked = false;
                        current.draft.notice = None;
                        self.overlay = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        match key.code {
            F(5) => self.refresh(),
            F(4) if self.signed_in => {
                if self.catalog_job.is_none() {
                    self.load_catalog();
                }
                self.model_selected = 0;
                self.overlay = Some(Overlay::Models);
            }
            Tab | BackTab => {
                self.focus = if self.focus == Focus::Draft {
                    Focus::Browser
                } else {
                    Focus::Draft
                };
            }
            Esc => self.focus = Focus::Browser,
            _ if self.focus == Focus::Browser => match key.code {
                Up | Down | PageUp | PageDown | Home | End => {
                    let length = self.nav_rows().len();
                    step(&mut self.nav_selected, length, key.code);
                }
                Enter => self.open_nav(),
                _ => {}
            },
            PageUp => self.scroll = self.scroll.saturating_add(8).min(32_000),
            PageDown => self.scroll = self.scroll.saturating_sub(8),
            _ if self.sending_here() => {}
            F(2) => self.submit(true),
            F(3) => self.submit(false),
            F(6) => self.insert("\n"),
            Enter => self.submit(self.enter_asks()),
            Left | Right | Backspace | Delete | Home | End => {
                let draft = &mut self.current_mut().draft;
                match key.code {
                    Left => draft.left(),
                    Right => draft.right(),
                    Backspace => draft.backspace(),
                    Delete => draft.delete(),
                    Home => draft.caret = 0,
                    End => draft.caret = draft.text.len(),
                    _ => unreachable!(),
                }
            }
            Char(c)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) && !c.is_control() =>
            {
                self.insert(c.encode_utf8(&mut [0; 4]))
            }
            _ => {}
        }
        true
    }
}

fn preflight(err: Failure) -> Failure {
    match err {
        Failure::Http(401) => err,
        _ => Failure::AccessCheck,
    }
}

fn step(index: &mut usize, length: usize, key: KeyCode) {
    use KeyCode::*;
    let last = length.saturating_sub(1);
    *index = match key {
        Up => index.saturating_sub(1),
        Down => (*index + 1).min(last),
        PageUp => index.saturating_sub(8),
        PageDown => (*index + 8).min(last),
        Home => 0,
        End => last,
        _ => *index,
    };
}
