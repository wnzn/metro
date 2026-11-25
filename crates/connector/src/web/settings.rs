//! Web search settings on this computer (`web-settings.json`, a private
//! file in the person's web profile; not secret).
//!
//! - `enabled`: the master switch, **on by default**; Settings shows a notice
//!   about what web search sends where until the person acknowledges it.
//! - `providers`: the try order and each provider's on/off, per computer.
//! - `cooldowns`: per provider and mode, until when it's skipped after a
//!   limit or outage, so a restart doesn't hammer a vendor.
//! - `rejected_keys`: the fingerprint of a key a provider rejected; that
//!   provider is skipped until the key changes (never silently keyless).
//! - `last`: each provider's latest outcome, for Settings.
//!
//! Two files, so the worker can never overwrite a person's choice:
//! - `web-settings.json`: the person's **preferences** (`enabled`,
//!   `notice_acknowledged`, `providers`), written only by their own actions
//!   ([`update_prefs`]);
//! - `web-state.json`: what jobs learned (`cooldowns`, `rejected_keys`,
//!   `last`), written by the worker ([`apply`]) and by "Try again now".
//!
//! Every read-modify-write holds an interprocess lock (`web.lock`), and a
//! file that can't be read is an error, never replaced by defaults. Readers
//! that can't read the preferences treat web search as turned off.
//!
//! The server never sees any of this.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Mode, Outcome, ProviderId};
use crate::fsutil::{read_private, write_private};

pub const FILE: &str = "web-settings.json";
pub(crate) const CONSENT_FILE: &str = "web-test-consent.json";

#[derive(Default, Serialize, Deserialize)]
struct ConsentRevisions {
    master: u64,
    providers: BTreeMap<String, u64>,
}

pub(super) type ConsentRevision = (u64, u64);

fn revisions(dir: &Path) -> Result<ConsentRevisions> {
    read_private(&dir.join(CONSENT_FILE), 4096)?
        .map(|bytes| {
            serde_json::from_slice(&bytes).context("web consent generations are unreadable")
        })
        .unwrap_or_else(|| Ok(ConsentRevisions::default()))
}

fn revision(dir: &Path, p: ProviderId) -> Result<ConsentRevision> {
    let r = revisions(dir)?;
    Ok((r.master, r.providers.get(p.as_str()).copied().unwrap_or(0)))
}

pub(super) fn test_snapshot(dir: &Path, p: ProviderId) -> Result<(Settings, ConsentRevision)> {
    locked(dir, || Ok((load(dir)?, revision(dir, p)?)))
}

fn advance_revisions(dir: &Path, master: bool, providers: &[ProviderId]) -> Result<()> {
    if !master && providers.is_empty() {
        return Ok(());
    }
    let mut r = revisions(dir)?;
    let next = |n: u64| n.checked_add(1).context("web consent generation exhausted");
    if master {
        r.master = next(r.master)?;
    }
    for p in providers {
        let entry = r.providers.entry(p.as_str().into()).or_default();
        *entry = next(*entry)?;
    }
    write_private(&dir.join(CONSENT_FILE), &serde_json::to_vec(&r)?)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSetting {
    pub id: ProviderId,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cooldown {
    pub until: u64,
    pub outcome: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Last {
    pub outcome: String,
    pub at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub enabled: bool,
    pub notice_acknowledged: bool,
    pub providers: Vec<ProviderSetting>,
    /// `provider:mode` → cooldown.
    pub cooldowns: BTreeMap<String, Cooldown>,
    /// provider → key fingerprint.
    pub rejected_keys: BTreeMap<String, String>,
    /// provider → latest outcome.
    pub last: BTreeMap<String, Last>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            notice_acknowledged: false,
            providers: ProviderId::ALL
                .iter()
                .map(|&id| ProviderSetting { id, enabled: true })
                .collect(),
            cooldowns: BTreeMap::new(),
            rejected_keys: BTreeMap::new(),
            last: BTreeMap::new(),
        }
    }
}

pub fn cooldown_key(p: ProviderId, mode: Mode) -> String {
    format!("{}:{}", p.as_str(), mode.as_str())
}

impl Settings {
    /// Until when `p` in `mode` is cooling down, if it still is at `now`.
    pub fn cooldown_until(&self, p: ProviderId, mode: Mode, now: u64) -> Option<u64> {
        self.cooldowns
            .get(&cooldown_key(p, mode))
            .map(|c| c.until)
            .filter(|u| *u > now)
    }

    pub fn key_rejected(&self, p: ProviderId, fingerprint: &str) -> bool {
        self.rejected_keys
            .get(p.as_str())
            .is_some_and(|f| f == fingerprint)
    }

    /// Set the order and on/off. Every provider must appear exactly once.
    pub fn set_providers(&mut self, list: &[(ProviderId, bool)]) -> Result<(), &'static str> {
        let mut seen = Vec::new();
        for (id, _) in list {
            if seen.contains(id) {
                return Err("each provider may appear once");
            }
            seen.push(*id);
        }
        if seen.len() != ProviderId::ALL.len() {
            return Err("list every provider once, in the order to try them");
        }
        self.providers = list
            .iter()
            .map(|&(id, enabled)| ProviderSetting { id, enabled })
            .collect();
        Ok(())
    }
}

pub const STATE_FILE: &str = "web-state.json";
pub const LOCK_FILE: &str = "web.lock";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefsWire {
    v: u32,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    notice_acknowledged: bool,
    #[serde(default)]
    providers: Vec<WireProvider>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct StateWire {
    #[serde(default)]
    v: u32,
    #[serde(default)]
    cooldowns: BTreeMap<String, Cooldown>,
    #[serde(default)]
    rejected_keys: BTreeMap<String, String>,
    #[serde(default)]
    last: BTreeMap<String, Last>,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProvider {
    id: String,
    enabled: bool,
}

fn read_prefs(dir: &Path, s: &mut Settings) -> Result<()> {
    let Some(bytes) = read_private(&dir.join(FILE), 256 * 1024)? else {
        return Ok(());
    };
    let w: PrefsWire = serde_json::from_slice(&bytes).context("web-settings.json is not valid")?;
    // A version this build doesn't know is unreadable: web search off.
    if w.v != 1 {
        anyhow::bail!("web-settings.json has an unsupported version");
    }
    let mut providers: Vec<ProviderSetting> = Vec::new();
    for p in &w.providers {
        if let Some(id) = ProviderId::parse(&p.id)
            && !providers.iter().any(|x| x.id == id)
        {
            providers.push(ProviderSetting {
                id,
                enabled: p.enabled,
            });
        }
    }
    for id in ProviderId::ALL {
        if !providers.iter().any(|x| x.id == id) {
            providers.push(ProviderSetting { id, enabled: true });
        }
    }
    s.enabled = w.enabled;
    s.notice_acknowledged = w.notice_acknowledged;
    s.providers = providers;
    Ok(())
}

fn read_state(dir: &Path, s: &mut Settings) -> Result<()> {
    let Some(bytes) = read_private(&dir.join(STATE_FILE), 1024 * 1024)? else {
        return Ok(());
    };
    let w: StateWire = serde_json::from_slice(&bytes).context("web-state.json is not valid")?;
    if w.v != 1 {
        anyhow::bail!("web-state.json has an unsupported version");
    }
    s.cooldowns = w.cooldowns;
    s.rejected_keys = w.rejected_keys;
    s.last = w.last;
    Ok(())
}

fn write_prefs(dir: &Path, s: &Settings) -> Result<()> {
    let w = PrefsWire {
        v: 1,
        enabled: s.enabled,
        notice_acknowledged: s.notice_acknowledged,
        providers: s
            .providers
            .iter()
            .map(|p| WireProvider {
                id: p.id.as_str().into(),
                enabled: p.enabled,
            })
            .collect(),
    };
    let mut json = serde_json::to_vec_pretty(&w)?;
    json.push(b'\n');
    write_private(&dir.join(FILE), &json)
}

fn write_state(dir: &Path, s: &Settings) -> Result<()> {
    let w = StateWire {
        v: 1,
        cooldowns: s.cooldowns.clone(),
        rejected_keys: s.rejected_keys.clone(),
        last: s.last.clone(),
    };
    let mut json = serde_json::to_vec_pretty(&w)?;
    json.push(b'\n');
    write_private(&dir.join(STATE_FILE), &json)
}

fn locked<T>(dir: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    crate::fsutil::ensure_private_dir(dir)?;
    let _lock = crate::fsutil::lock_exclusive(&dir.join(LOCK_FILE))?;
    f()
}

/// The settings in `dir`: defaults where a file is missing; an error (never
/// defaults) when a file exists but can't be read. Unknown or repeated
/// providers are dropped and missing ones appended in the default order,
/// enabled.
pub fn load(dir: &Path) -> Result<Settings> {
    let mut s = Settings::default();
    read_prefs(dir, &mut s)?;
    read_state(dir, &mut s)?;
    Ok(s)
}

/// Which file can't be read: `(preferences, job state)`.
pub fn unreadable(dir: &Path) -> (bool, bool) {
    let mut s = Settings::default();
    (
        read_prefs(dir, &mut s).is_err(),
        read_state(dir, &mut s).is_err(),
    )
}

/// Save everything (setup and tests). Under the lock.
pub fn save(dir: &Path, s: &Settings) -> Result<()> {
    locked(dir, || {
        write_prefs(dir, s)?;
        write_state(dir, s)
    })
}

/// Change the person's preferences: read (strictly), `f`, write the
/// preferences only, all under the lock. Changes `f` makes to the state
/// fields are discarded.
///
/// An unreadable preferences file is an error and left as it is, except
/// with `reset_unreadable`, which a caller uses only to turn web search
/// *off* (starting from defaults with the switch off is always safe).
pub fn update_prefs<T>(
    dir: &Path,
    reset_unreadable: bool,
    f: impl FnOnce(&mut Settings) -> Result<T>,
) -> Result<T> {
    locked(dir, || {
        let mut s = Settings::default();
        if let Err(e) = read_prefs(dir, &mut s) {
            if !reset_unreadable {
                return Err(e);
            }
            s = Settings {
                enabled: false,
                ..Settings::default()
            };
        }
        let before = s.clone();
        let out = f(&mut s)?;
        let enabled = |settings: &Settings, p| {
            settings
                .providers
                .iter()
                .any(|row| row.id == p && row.enabled)
        };
        let changed: Vec<_> = ProviderId::ALL
            .into_iter()
            .filter(|&p| enabled(&before, p) != enabled(&s, p))
            .collect();
        // Advance before writing preferences: even a subsequent failed
        // preference write must not revive an earlier test. Never make Off
        // depend on the revision file, but don't re-enable without it.
        let bumped = advance_revisions(
            dir,
            reset_unreadable || before.enabled != s.enabled,
            &changed,
        );
        if (!before.enabled && s.enabled) || changed.iter().any(|&p| enabled(&s, p)) {
            bumped?;
        }
        write_prefs(dir, &s)?;
        Ok(out)
    })
}

/// Change what jobs learned (cooldowns, rejected keys, outcomes); never the
/// preferences. Strict: an unreadable state file is left as it is. With
/// `reset_unreadable` (an explicit person's action), it starts over.
pub fn update_state<T>(
    dir: &Path,
    reset_unreadable: bool,
    f: impl FnOnce(&mut Settings) -> Result<T>,
) -> Result<T> {
    locked(dir, || {
        let mut s = Settings::default();
        if let Err(e) = read_state(dir, &mut s) {
            if !reset_unreadable {
                return Err(e);
            }
            s = Settings::default();
        }
        let out = f(&mut s)?;
        write_state(dir, &s)?;
        Ok(out)
    })
}

/// What one job learned, applied to the file as it is now (other jobs may
/// have written meanwhile).
#[derive(Default, Debug)]
pub struct Updates {
    pub cooldowns: Vec<(ProviderId, Mode, u64, Outcome)>,
    pub rejected: Vec<(ProviderId, String)>,
    pub last: Vec<(ProviderId, Outcome, u64)>,
    /// Provider modes whose cooldowns end now (a successful test).
    pub cleared: Vec<(ProviderId, Mode)>,
}

impl Updates {
    pub fn is_empty(&self) -> bool {
        self.cooldowns.is_empty()
            && self.rejected.is_empty()
            && self.last.is_empty()
            && self.cleared.is_empty()
    }
    fn apply_to(&self, s: &mut Settings) {
        for &(p, mode, until, outcome) in &self.cooldowns {
            s.cooldowns.insert(
                cooldown_key(p, mode),
                Cooldown {
                    until,
                    outcome: outcome.as_str().into(),
                },
            );
        }
        for (p, fp) in &self.rejected {
            s.rejected_keys.insert(p.as_str().into(), fp.clone());
        }
        for &(p, outcome, at) in &self.last {
            s.last.insert(
                p.as_str().into(),
                Last {
                    outcome: outcome.as_str().into(),
                    at,
                },
            );
        }
        for &(p, mode) in &self.cleared {
            s.cooldowns.remove(&cooldown_key(p, mode));
        }
    }
}

pub fn apply(dir: &Path, u: &Updates) -> Result<()> {
    if u.is_empty() {
        return Ok(());
    }
    update_state(dir, false, |s| {
        u.apply_to(s);
        Ok(())
    })
}

/// Publish a test only if its final checks pass while we hold the same
/// lock as preference changes and cooldown writes. The caller holds the
/// key-store lock first and keeps it until this transaction ends.
pub(super) fn apply_test<G>(
    dir: &Path,
    p: ProviderId,
    u: &Updates,
    verify: impl FnOnce(&Settings, ConsentRevision) -> Result<G>,
) -> Result<()> {
    locked(dir, || {
        let mut s = load(dir)?;
        let _publication = verify(&s, revision(dir, p)?)?;
        if !u.is_empty() {
            u.apply_to(&mut s);
            write_state(dir, &s)?;
        }
        Ok(())
    })
}

/// End every cooldown of `p` ("Try again now").
pub fn clear_cooldowns(s: &mut Settings, p: ProviderId) {
    s.cooldowns
        .retain(|k, _| k.split(':').next() != Some(p.as_str()));
}
