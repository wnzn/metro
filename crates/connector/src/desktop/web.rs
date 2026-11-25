//! Web search in the desktop app: this computer as the signed-in person's
//! web device.
//!
//! - **On by default, with a notice.** When a session is bound and the
//!   person hasn't turned web search off on this computer, the app
//!   registers this computer with Metro (`POST /v1/web-devices`, minting
//!   its own device token with the session) and serves web jobs. Settings
//!   shows a notice until the person acknowledges it. Agents still only
//!   search when a Space owner allowed it, and personal Chat only after the
//!   person's own opt-in (both on the server).
//! - **Off** stops the workers and removes this computer from Metro
//!   (`DELETE /v1/web-devices/{id}`, which revokes its token); on again
//!   registers it anew.
//! - Everything is per (server, account) in its own private profile: two
//!   accounts on one computer never share keys, order or cooldowns.
//! - Keys are write-only here: the UI only ever learns whether one is saved.
//! - Workers run only while the session is bound, on the primary instance,
//!   and stop with it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::core::{BridgeError, WorkerState, err};
use super::metro_api::{ApiError, MetroApi};
use super::monitoring::{EventKind, Events};
use super::store;
use crate::metro::MetroClient;
use crate::profile::Profile;
use crate::run::{RunOptions, RunState, Stop};
use crate::secret::clean_line;
use crate::web::binding::{self, Binding};
use crate::web::{self, Endpoints, Engine, Mode, ProviderId, keys, settings};
use crate::{SafeError, Secret};

/// The pseudo connection id the web profile name is derived from.
const WEB_PROFILE: &str = "web-search";
/// Web jobs this computer runs at once.
pub const WORKERS: usize = web::WORKERS_PER_DEVICE;

type R<T> = Result<T, BridgeError>;

fn owner(account: &str) -> String {
    format!("account:{account}")
}

#[derive(Default)]
struct Live {
    epoch: u64,
    worker: WorkerState,
    detail: Option<String>,
    stop: Option<watch::Sender<bool>>,
    tasks: Vec<JoinHandle<()>>,
    engine: Option<Arc<Engine>>,
}

pub struct WebSearch {
    root: PathBuf,
    primary: bool,
    poll_interval: Duration,
    endpoints: Result<Endpoints, String>,
    events: Events,
    live: Arc<Mutex<Live>>,
    /// The lifecycle lock: binding, ending, and every change. Holds the
    /// (server, account) of the bound session, `None` after sign-out, so a
    /// change that captured an older session before waiting here is refused
    /// instead of pairing or starting workers for nobody.
    ops: tokio::sync::Mutex<Option<(String, String, u64)>>,
}

fn lock(m: &Mutex<Live>) -> std::sync::MutexGuard<'_, Live> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn api_err(e: ApiError) -> BridgeError {
    match e {
        ApiError::Rejected => err(
            422,
            "session_rejected",
            "The Metro server did not accept this sign-in; sign in again",
        ),
        ApiError::NotFound => err(404, "not_found", "Metro doesn't know this computer"),
        ApiError::Unreachable => err(502, "server_unreachable", "Cannot reach the Metro server"),
        ApiError::Server(m) => err(502, "server_error", m),
    }
}

fn local(e: anyhow::Error, what: &str) -> BridgeError {
    let msg = e
        .downcast_ref::<SafeError>()
        .or_else(|| e.chain().find_map(|c| c.downcast_ref::<SafeError>()))
        .map(|s| s.as_str().to_owned())
        .unwrap_or_else(|| what.to_owned());
    err(500, "local_error", msg)
}

fn provider(id: &str) -> R<ProviderId> {
    ProviderId::parse(id).ok_or_else(|| err(404, "not_found", "No such web search provider"))
}

/// A label for this computer, as Metro lists it.
fn device_label() -> String {
    let os = if cfg!(windows) {
        "Windows"
    } else if cfg!(target_os = "macos") {
        "macOS"
    } else {
        "Linux"
    };
    format!("Metro desktop ({os})")
}

impl WebSearch {
    pub fn new(root: &Path, primary: bool, poll_interval: Duration, events: Events) -> Self {
        Self {
            root: root.to_path_buf(),
            primary,
            poll_interval,
            endpoints: Endpoints::from_env().map_err(|e| clean_line(&e.to_string(), 200)),
            events,
            live: Arc::new(Mutex::new(Live::default())),
            ops: tokio::sync::Mutex::new(None),
        }
    }

    /// For tests: vendor URLs replaced (loopback fixtures).
    pub fn with_endpoints(mut self, e: Endpoints) -> Self {
        self.endpoints = Ok(e);
        self
    }

    fn profile(&self, server: &str, account: &str) -> R<Profile> {
        let p = store::profile_for(&self.root, server, account, WEB_PROFILE)
            .map_err(|e| local(e, "The web search profile can't be opened"))?;
        p.ensure()
            .map_err(|e| local(e, "The web search profile can't be created"))?;
        Ok(p)
    }

    /// This profile's binding, only when it is exactly (server, account):
    /// identity and token from the same record.
    fn binding(p: &Profile, server: &str, account: &str) -> Option<Binding> {
        binding::load(&p.dir)
            .ok()
            .flatten()
            .filter(|b| b.token_for(server, &owner(account)).is_ok() && b.device_id.is_some())
    }

    fn engine(&self, p: &Profile) -> R<Arc<Engine>> {
        if let Some(e) = lock(&self.live).engine.clone()
            && e.dir() == p.dir
        {
            return Ok(e);
        }
        let endpoints = self
            .endpoints
            .clone()
            .map_err(|m| err(500, "local_error", m))?;
        Engine::new(&p.dir, endpoints)
            .map(Arc::new)
            .map_err(|e| local(e, "Web search can't start on this computer"))
    }

    /// Register this computer with Metro if it isn't yet (or its token is
    /// gone). Returns the device id.
    async fn ensure_paired(
        &self,
        p: &Profile,
        server: &str,
        account: &str,
        session: &Secret,
    ) -> R<String> {
        if let Some(id) = Self::binding(p, server, account).and_then(|b| b.device_id) {
            return Ok(id);
        }
        let api = MetroApi::new(server, session.clone()).map_err(api_err)?;
        let (id, token) = api
            .create_web_device(&device_label())
            .await
            .map_err(|e| match e {
                ApiError::NotFound => err(
                    409,
                    "unsupported",
                    "This Metro server doesn't offer web search yet",
                ),
                e => api_err(e),
            })?;
        // One atomic record; a registration that can't be saved here is
        // removed again so it doesn't linger in Metro.
        let saved = Binding::new(server, &owner(account), Some(id.clone()), token)
            .and_then(|b| binding::save(&p.dir, &b));
        if let Err(e) = saved {
            let _ = api.delete_web_device(&id).await;
            return Err(local(e, "The web device can't be saved"));
        }
        Ok(id)
    }

    /// Remove this computer from Metro (best effort) and forget its token.
    async fn unpair(
        &self,
        p: &Profile,
        server: &str,
        account: &str,
        session: &Secret,
    ) -> Option<String> {
        let mut problem = None;
        if let Some(b) = Self::binding(p, server, account) {
            match MetroApi::new(server, session.clone()) {
                Ok(api) => match api
                    .delete_web_device(b.device_id.as_deref().unwrap_or_default())
                    .await
                {
                    Ok(()) | Err(ApiError::NotFound) => {}
                    Err(e) => problem = Some(api_err(e).message),
                },
                Err(e) => problem = Some(api_err(e).message),
            }
        }
        let _ = binding::remove(&p.dir);
        problem
    }

    fn set_state(&self, epoch: u64, w: WorkerState, d: Option<String>) {
        let mut l = lock(&self.live);
        if l.epoch == epoch {
            l.worker = w;
            l.detail = d;
        }
    }

    fn spawn(&self, server: &str, token: Secret, engine: Arc<Engine>) {
        let (tx, rx) = watch::channel(false);
        let epoch = {
            let mut l = lock(&self.live);
            l.epoch += 1;
            l.engine = Some(engine.clone());
            l.worker = WorkerState::Starting;
            l.detail = None;
            l.epoch
        };
        let client = match MetroClient::new(server, token, Duration::from_secs(30)) {
            Ok(c) => Arc::new(c),
            Err(_) => {
                self.set_state(
                    epoch,
                    WorkerState::Error,
                    Some("cannot start the HTTP client".into()),
                );
                return;
            }
        };
        let mut tasks = Vec::new();
        for _ in 0..WORKERS {
            let live = self.live.clone();
            let on_state = Arc::new(move |rs: RunState| {
                let (w, d) = match rs {
                    RunState::Online => (WorkerState::Online, None),
                    RunState::Offline => (
                        WorkerState::Offline,
                        Some("Cannot reach the Metro server; retrying".to_owned()),
                    ),
                };
                let mut l = lock(&live);
                if l.epoch == epoch {
                    l.worker = w;
                    l.detail = d;
                }
            });
            let opts = RunOptions {
                poll_interval: self.poll_interval,
                once: false,
                stop: Some(rx.clone()),
                on_state: Some(on_state),
            };
            let (client, engine, live, events) = (
                client.clone(),
                engine.clone(),
                self.live.clone(),
                self.events.clone(),
            );
            tasks.push(tokio::spawn(async move {
                let (w, d) = match web::worker::run(&client, &engine, &opts).await {
                    Stop::Done => return,
                    Stop::Unauthorized => {
                        events.record(EventKind::WebWorkerUnauthorized);
                        (
                            WorkerState::Unauthorized,
                            Some("Metro no longer accepts this computer for web search; turn web search off and on again".to_owned()),
                        )
                    }
                    Stop::Fatal(m) => {
                        events.record(EventKind::WebWorkerFailed);
                        (WorkerState::Error, Some(clean_line(&m, 200)))
                    }
                };
                let mut l = lock(&live);
                if l.epoch == epoch {
                    l.worker = w;
                    l.detail = d;
                }
            }));
        }
        let mut l = lock(&self.live);
        l.stop = Some(tx);
        l.tasks = tasks;
    }

    /// Stop the workers (bounded wait).
    pub async fn stop(&self) {
        let tasks = {
            let mut l = lock(&self.live);
            l.epoch += 1;
            if let Some(tx) = l.stop.take() {
                let _ = tx.send(true);
            }
            l.worker = WorkerState::Stopped;
            l.detail = None;
            l.engine = None;
            std::mem::take(&mut l.tasks)
        };
        for mut t in tasks {
            if tokio::time::timeout(Duration::from_secs(5), &mut t)
                .await
                .is_err()
            {
                t.abort();
            }
        }
    }

    async fn start_locked(&self, server: &str, account: &str, session: &Secret) -> R<()> {
        self.stop().await;
        if !self.primary {
            return Ok(());
        }
        let p = self.profile(server, account)?;
        let s = settings::load(&p.dir).unwrap_or_default();
        if !s.enabled {
            return Ok(());
        }
        let engine = self.engine(&p)?;
        let paired = self.ensure_paired(&p, server, account, session).await;
        if let Err(e) = paired {
            let mut l = lock(&self.live);
            l.worker = WorkerState::Error;
            l.detail = Some(format!(
                "Couldn't set up web search with Metro: {}",
                e.message
            ));
            return Err(e);
        }
        let token = Self::binding(&p, server, account)
            .map(|b| b.token().clone())
            .ok_or_else(|| err(500, "local_error", "The device token can't be read"))?;
        self.spawn(server, token, engine);
        Ok(())
    }

    /// The lifecycle lock, if `(server, account)` is still the bound session.
    async fn bound_ops(
        &self,
        server: &str,
        account: &str,
        epoch: u64,
    ) -> R<tokio::sync::MutexGuard<'_, Option<(String, String, u64)>>> {
        let g = self.ops.lock().await;
        match &*g {
            Some((s, a, e)) if s == server && a == account && *e == epoch => Ok(g),
            _ => Err(err(409, "no_session", "Sign in to a Metro server first")),
        }
    }

    /// A session was bound: serve web search for it, unless it's off here.
    pub async fn session_bound(&self, server: &str, account: &str, session: &Secret, epoch: u64) {
        let mut g = self.ops.lock().await;
        *g = Some((server.to_owned(), account.to_owned(), epoch));
        let _ = self.start_locked(server, account, session).await;
    }

    /// Sign-out, account switch or exit.
    pub async fn session_ended(&self) {
        let mut g = self.ops.lock().await;
        *g = None;
        self.stop().await;
    }

    fn provider_row(
        &self,
        s: &settings::Settings,
        k: &Result<keys::Keys, String>,
        p: ProviderId,
        now: u64,
    ) -> Value {
        let ps = s.providers.iter().find(|x| x.id == p);
        let key = k.as_ref().ok().and_then(|k| k.get(p.as_str()));
        let mode = if key.is_some() {
            Mode::Key
        } else {
            Mode::Keyless
        };
        let key_status = key.map(|k| {
            if s.key_rejected(p, &keys::fingerprint(k)) {
                "rejected"
            } else {
                "ok"
            }
        });
        let cooldown = s
            .cooldowns
            .get(&settings::cooldown_key(p, mode))
            .filter(|c| c.until > now)
            .map(|c| json!({"mode": mode.as_str(), "until": web::rfc3339(c.until), "outcome": c.outcome}));
        let last = s
            .last
            .get(p.as_str())
            .map(|l| json!({"outcome": l.outcome, "at": web::rfc3339(l.at)}));
        json!({
            "id": p.as_str(),
            "name": p.name(),
            "enabled": ps.is_none_or(|x| x.enabled),
            "keyless": p.keyless(),
            "key_saved": key.is_some(),
            "key_status": key_status,
            "cooldown": cooldown,
            "last": last,
        })
    }

    fn view(&self, p: &Profile, server: &str, account: &str) -> Value {
        let s = settings::load(&p.dir).unwrap_or_default();
        let k = keys::KeyStore::new(&p.dir)
            .load()
            .map_err(|e| local(e, "the saved keys can't be read").message);
        let now = web::time_now();
        let b = Self::binding(p, server, account);
        let (worker, detail) = {
            let l = lock(&self.live);
            (l.worker, l.detail.clone())
        };
        let providers: Vec<Value> = s
            .providers
            .iter()
            .map(|ps| self.provider_row(&s, &k, ps.id, now))
            .collect();
        json!({
            "enabled": s.enabled,
            "notice_acknowledged": s.notice_acknowledged,
            "device": {
                "paired": b.is_some(),
                "id": b.and_then(|b| b.device_id),
                "worker": worker,
                "detail": detail,
            },
            "keys_error": k.as_ref().err(),
            "endpoints_error": self.endpoints.as_ref().err(),
            "providers": providers,
        })
    }

    /// `GET /v1/web-search`.
    pub async fn state(&self, server: &str, account: &str) -> R<Value> {
        let p = self.profile(server, account)?;
        Ok(self.view(&p, server, account))
    }

    /// `PUT /v1/web-search`.
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        server: &str,
        account: &str,
        epoch: u64,
        session: &Secret,
        enabled: Option<bool>,
        notice_acknowledged: Option<bool>,
        providers: Option<Vec<(String, bool)>>,
    ) -> R<Value> {
        let _g = self.bound_ops(server, account, epoch).await?;
        let p = self.profile(server, account)?;
        let list = match providers {
            Some(list) => Some(
                list.iter()
                    .map(|(id, on)| provider(id).map(|p| (p, *on)))
                    .collect::<R<Vec<_>>>()?,
            ),
            None => None,
        };
        let before = settings::load(&p.dir).unwrap_or_default();
        let engine = self.engine(&p)?;
        let changed = engine
            .edit(|dir| {
                let mut s = settings::load(dir).unwrap_or_default();
                if let Some(list) = &list {
                    s.set_providers(list)
                        .map_err(|m| anyhow::anyhow!(SafeError::new(m)))?;
                }
                if let Some(a) = notice_acknowledged {
                    s.notice_acknowledged = a;
                }
                if let Some(e) = enabled {
                    s.enabled = e;
                }
                settings::save(dir, &s)?;
                Ok(s.enabled != before.enabled)
            })
            .await
            .map_err(|e| {
                let b = local(e, "The settings can't be saved");
                err(422, "invalid", b.message)
            })?;
        if changed {
            if enabled == Some(false) {
                self.stop().await;
                if let Some(problem) = self.unpair(&p, server, account, session).await {
                    let mut l = lock(&self.live);
                    l.detail = Some(format!(
                        "Turned off here, but Metro couldn't be told ({problem}); it will see this computer as offline"
                    ));
                }
            } else {
                self.start_locked(server, account, session).await?;
            }
        }
        Ok(self.view(&p, server, account))
    }

    /// `PUT /v1/web-search/providers/{id}/key`: saved on this computer only.
    pub async fn set_key(
        &self,
        server: &str,
        account: &str,
        epoch: u64,
        id: &str,
        key: Secret,
    ) -> R<Value> {
        let _g = self.bound_ops(server, account, epoch).await?;
        let pid = provider(id)?;
        let p = self.profile(server, account)?;
        keys::validate_key(&key).map_err(|e| err(422, "invalid", e.as_str()))?;
        let engine = self.engine(&p)?;
        engine
            .edit(|dir| keys::KeyStore::new(dir).set(pid, key))
            .await
            .map_err(|e| local(e, "The key can't be saved on this computer"))?;
        self.row(&p, pid)
    }

    /// `DELETE /v1/web-search/providers/{id}/key`: back to the free mode.
    pub async fn delete_key(&self, server: &str, account: &str, epoch: u64, id: &str) -> R<Value> {
        let _g = self.bound_ops(server, account, epoch).await?;
        let pid = provider(id)?;
        let p = self.profile(server, account)?;
        let engine = self.engine(&p)?;
        engine
            .edit(|dir| keys::KeyStore::new(dir).delete(pid).map(|_| ()))
            .await
            .map_err(|e| local(e, "The key can't be deleted"))?;
        self.row(&p, pid)
    }

    /// `POST /v1/web-search/providers/{id}/test`: one fixed query, now.
    pub async fn test(&self, server: &str, account: &str, epoch: u64, id: &str) -> R<Value> {
        // Checked, not held: a vendor call mustn't delay a sign-out.
        drop(self.bound_ops(server, account, epoch).await?);
        let pid = provider(id)?;
        let p = self.profile(server, account)?;
        let engine = self.engine(&p)?;
        let outcome = engine.test_provider(pid).await;
        let mut row = self.row(&p, pid)?;
        row["test"] = json!({"outcome": outcome.as_str()});
        Ok(row)
    }

    /// `POST /v1/web-search/providers/{id}/clear-cooldown` ("Try again now").
    pub async fn clear_cooldown(
        &self,
        server: &str,
        account: &str,
        epoch: u64,
        id: &str,
    ) -> R<Value> {
        let _g = self.bound_ops(server, account, epoch).await?;
        let pid = provider(id)?;
        let p = self.profile(server, account)?;
        let engine = self.engine(&p)?;
        engine
            .edit(|dir| {
                let mut s = settings::load(dir).unwrap_or_default();
                settings::clear_cooldowns(&mut s, pid);
                s.rejected_keys.remove(pid.as_str());
                settings::save(dir, &s)
            })
            .await
            .map_err(|e| local(e, "The settings can't be saved"))?;
        self.row(&p, pid)
    }

    fn row(&self, p: &Profile, pid: ProviderId) -> R<Value> {
        let s = settings::load(&p.dir).unwrap_or_default();
        let k = keys::KeyStore::new(&p.dir)
            .load()
            .map_err(|e| local(e, "the saved keys can't be read").message);
        Ok(self.provider_row(&s, &k, pid, web::time_now()))
    }
}
