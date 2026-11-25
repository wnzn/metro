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
//! The server never sees any of this.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Mode, Outcome, ProviderId};
use crate::fsutil::{read_private, write_private};

pub const FILE: &str = "web-settings.json";

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

#[derive(Serialize, Deserialize)]
struct Wire {
    v: u32,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    notice_acknowledged: bool,
    #[serde(default)]
    providers: Vec<WireProvider>,
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
struct WireProvider {
    id: String,
    enabled: bool,
}

/// The settings in `dir`, or the defaults when there are none. Unknown or
/// repeated providers are dropped and missing ones appended in the default
/// order, enabled.
pub fn load(dir: &Path) -> Result<Settings> {
    let Some(bytes) = read_private(&dir.join(FILE), 256 * 1024)? else {
        return Ok(Settings::default());
    };
    let w: Wire = serde_json::from_slice(&bytes).context("web-settings.json is not valid")?;
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
    Ok(Settings {
        enabled: w.enabled,
        notice_acknowledged: w.notice_acknowledged,
        providers,
        cooldowns: w.cooldowns,
        rejected_keys: w.rejected_keys,
        last: w.last,
    })
}

pub fn save(dir: &Path, s: &Settings) -> Result<()> {
    crate::fsutil::ensure_private_dir(dir)?;
    let w = Wire {
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
        cooldowns: s.cooldowns.clone(),
        rejected_keys: s.rejected_keys.clone(),
        last: s.last.clone(),
    };
    let mut json = serde_json::to_vec_pretty(&w)?;
    json.push(b'\n');
    write_private(&dir.join(FILE), &json)
}

/// What one job learned, applied to the file as it is now (other jobs may
/// have written meanwhile).
#[derive(Default, Debug)]
pub struct Updates {
    pub cooldowns: Vec<(ProviderId, Mode, u64, Outcome)>,
    pub rejected: Vec<(ProviderId, String)>,
    pub last: Vec<(ProviderId, Outcome, u64)>,
    /// Providers whose cooldowns end now (a successful test).
    pub cleared: Vec<ProviderId>,
}

impl Updates {
    pub fn is_empty(&self) -> bool {
        self.cooldowns.is_empty()
            && self.rejected.is_empty()
            && self.last.is_empty()
            && self.cleared.is_empty()
    }
}

pub fn apply(dir: &Path, u: &Updates) -> Result<()> {
    if u.is_empty() {
        return Ok(());
    }
    let mut s = load(dir).unwrap_or_default();
    for &(p, mode, until, outcome) in &u.cooldowns {
        s.cooldowns.insert(
            cooldown_key(p, mode),
            Cooldown {
                until,
                outcome: outcome.as_str().into(),
            },
        );
    }
    for (p, fp) in &u.rejected {
        s.rejected_keys.insert(p.as_str().into(), fp.clone());
    }
    for &(p, outcome, at) in &u.last {
        s.last.insert(
            p.as_str().into(),
            Last {
                outcome: outcome.as_str().into(),
                at,
            },
        );
    }
    for p in &u.cleared {
        clear_cooldowns(&mut s, *p);
    }
    save(dir, &s)
}

/// End every cooldown of `p` ("Try again now").
pub fn clear_cooldowns(s: &mut Settings, p: ProviderId) {
    s.cooldowns
        .retain(|k, _| k.split(':').next() != Some(p.as_str()));
}
