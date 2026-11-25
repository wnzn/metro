//! Web search on this computer, for the person signed in here.
//!
//! Metro's server sends `web_search` and `web_fetch` jobs to a person's own
//! *web device* (this computer) through the device relay. They run here
//! with this person's providers, keys and IP address, and the answer goes
//! back to Metro. Nothing here ever runs for anyone else.
//!
//! - **Providers**: Tavily, Exa, Parallel, Firecrawl and TinyFish, tried in
//!   the person's order ([`settings`]). Exa and Parallel use their hosted
//!   MCP servers ([`mcp`]); the others REST. Each base URL is a compiled-in
//!   vendor constant; a loopback test override exists only through
//!   `METRO_WEB_TEST_BASE_<PROVIDER>`.
//! - **Keys** ([`keys`]) stay on this computer, in a private file (mode 600,
//!   DPAPI on Windows), are never sent to Metro, never logged and never
//!   returned. A provider with a saved key is used only with it; without one
//!   it's used free (keyless), except TinyFish, which needs a key.
//! - **Rotation** reacts to errors only (never counts requests): a limit,
//!   an exhausted credit or an outage puts that provider and mode on a
//!   cooldown and the next one is tried. Every attempt goes back to Metro
//!   as a fixed outcome word, so no vendor text leaves this computer.
//! - **Untrusted both ways**: the request is checked again here (lengths,
//!   URL rules) and only the compiled-in vendor hosts are contacted, so
//!   Metro can't use this computer as a proxy. Vendor answers are read
//!   with a size cap, never through redirects, and cut to Metro's limits.
//! - **Logs** name the job, operation, provider, mode and outcome only:
//!   never a query, URL, key or body.

pub mod binding;
pub mod cli;
pub mod keys;
pub mod mcp;
pub mod settings;
pub mod worker;

mod exa;
mod firecrawl;
mod parallel;
mod tavily;
mod time;
mod tinyfish;

#[cfg(test)]
mod tests;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use metro_model_protocol::web_privacy;
use serde_json::{Map, Value, json};

use crate::netpolicy::{url_is_loopback, validate_base_url};
use crate::secret::clean_line;
use crate::{SafeError, Secret};

pub use time::rfc3339;

/// Web jobs one device runs at once (separate loops, so a long fetch never
/// blocks a search).
pub const WORKERS_PER_DEVICE: usize = 2;

/// Wall-clock seconds since the epoch.
pub fn time_now() -> u64 {
    time::now_secs()
}

/// The providers, in the default try order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProviderId {
    Tavily,
    Exa,
    Parallel,
    Firecrawl,
    TinyFish,
}

impl ProviderId {
    pub const ALL: [ProviderId; 5] = [
        ProviderId::Tavily,
        ProviderId::Exa,
        ProviderId::Parallel,
        ProviderId::Firecrawl,
        ProviderId::TinyFish,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Tavily => "tavily",
            ProviderId::Exa => "exa",
            ProviderId::Parallel => "parallel",
            ProviderId::Firecrawl => "firecrawl",
            ProviderId::TinyFish => "tinyfish",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ProviderId::Tavily => "Tavily",
            ProviderId::Exa => "Exa",
            ProviderId::Parallel => "Parallel",
            ProviderId::Firecrawl => "Firecrawl",
            ProviderId::TinyFish => "TinyFish",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Works without a key (at the vendor's free, per-IP limits).
    pub fn keyless(self) -> bool {
        self != ProviderId::TinyFish
    }

    fn env_suffixes(self) -> &'static [&'static str] {
        match self {
            ProviderId::Tavily => &["TAVILY"],
            ProviderId::Exa => &["EXA"],
            ProviderId::Parallel => &["PARALLEL"],
            ProviderId::Firecrawl => &["FIRECRAWL"],
            ProviderId::TinyFish => &["TINYFISH_SEARCH", "TINYFISH_FETCH"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Keyless,
    Key,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Keyless => "keyless",
            Mode::Key => "key",
        }
    }
}

/// One attempt's outcome: a fixed vocabulary Metro understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Limited,
    OutOfCredits,
    KeyRejected,
    Blocked,
    Unavailable,
    Timeout,
    BadResponse,
    SkippedCooldown,
    SkippedNoKey,
    /// Turned off (the master switch or this provider) while the job ran:
    /// no further request was made.
    SkippedDisabled,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Limited => "limited",
            Outcome::OutOfCredits => "out_of_credits",
            Outcome::KeyRejected => "key_rejected",
            Outcome::Blocked => "blocked",
            Outcome::Unavailable => "unavailable",
            Outcome::Timeout => "timeout",
            Outcome::BadResponse => "bad_response",
            Outcome::SkippedCooldown => "skipped_cooldown",
            Outcome::SkippedNoKey => "skipped_no_key",
            Outcome::SkippedDisabled => "skipped_disabled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Outcome::Ok,
            Outcome::Limited,
            Outcome::OutOfCredits,
            Outcome::KeyRejected,
            Outcome::Blocked,
            Outcome::Unavailable,
            Outcome::Timeout,
            Outcome::BadResponse,
            Outcome::SkippedCooldown,
            Outcome::SkippedNoKey,
            Outcome::SkippedDisabled,
        ]
        .into_iter()
        .find(|o| o.as_str() == s)
    }

    /// The cooldown when the provider gave no hint.
    fn default_cooldown(self) -> Option<u64> {
        match self {
            Outcome::Limited => Some(10 * 60),
            Outcome::OutOfCredits => Some(6 * 3600),
            Outcome::Blocked => Some(3600),
            Outcome::Unavailable | Outcome::Timeout | Outcome::BadResponse => Some(60),
            _ => None,
        }
    }
}

/// How one provider call failed, with the provider's cooldown hint (seconds
/// from now), if it gave one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure {
    pub outcome: Outcome,
    pub cooldown: Option<u64>,
}

impl Failure {
    pub fn of(outcome: Outcome) -> Self {
        Self {
            outcome,
            cooldown: None,
        }
    }

    pub fn for_secs(outcome: Outcome, secs: u64) -> Self {
        Self {
            outcome,
            cooldown: Some(secs),
        }
    }
}

// ---------------------------------------------------------------------------
// Requests (checked again here: the server is not trusted beyond the schema).

pub const MAX_QUERY_CHARS: usize = 400;
pub const MAX_KEYWORDS: usize = 3;
pub const MAX_KEYWORD_CHARS: usize = 100;
pub const MAX_RESULTS: u64 = 10;
pub const MAX_URLS: usize = 5;
pub const MAX_URL_BYTES: usize = 2048;
pub const MAX_FOCUS_CHARS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchReq {
    pub query: String,
    pub keywords: Vec<String>,
    pub max_results: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchReq {
    pub urls: Vec<String>,
    pub focus: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Op {
    Search(SearchReq),
    Fetch(FetchReq),
}

impl Op {
    pub fn name(&self) -> &'static str {
        match self {
            Op::Search(_) => "web_search",
            Op::Fetch(_) => "web_fetch",
        }
    }
}

fn bad(msg: impl AsRef<str>) -> SafeError {
    SafeError::new(format!("invalid web request: {}", msg.as_ref()))
}

fn text_field(v: Option<&Value>, what: &str, max: usize) -> Result<String, SafeError> {
    let s = v
        .and_then(Value::as_str)
        .ok_or_else(|| bad(format!("{what} must be a string")))?
        .trim();
    if s.is_empty() || s.chars().count() > max || s.chars().any(char::is_control) {
        return Err(bad(format!("{what} must be 1..={max} characters")));
    }
    Ok(s.to_owned())
}

fn only(obj: &Map<String, Value>, known: &[&str]) -> Result<(), SafeError> {
    match obj.keys().find(|k| !known.contains(&k.as_str())) {
        Some(_) => Err(bad("unknown field")),
        None => Ok(()),
    }
}

/// Parse a claimed job. Only `web_search` and `web_fetch`; anything else is
/// refused (a web device serves nothing else).
pub fn parse_job(operation: &str, request: &Value) -> Result<Op, SafeError> {
    let obj = request
        .as_object()
        .ok_or_else(|| bad("'request' must be an object"))?;
    if obj.get("v") != Some(&json!(1)) {
        return Err(bad("unsupported version"));
    }
    match operation {
        "web_search" => {
            only(obj, &["v", "query", "keywords", "max_results"])?;
            let query = text_field(obj.get("query"), "query", MAX_QUERY_CHARS)?;
            let keywords = match obj.get("keywords") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) if a.len() <= MAX_KEYWORDS => a
                    .iter()
                    .map(|k| text_field(Some(k), "keyword", MAX_KEYWORD_CHARS))
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err(bad("keywords")),
            };
            let max_results = match obj.get("max_results") {
                None | Some(Value::Null) => 5,
                Some(v) => v
                    .as_u64()
                    .filter(|n| (1..=MAX_RESULTS).contains(n))
                    .ok_or_else(|| bad("max_results"))?,
            } as u32;
            Ok(Op::Search(SearchReq {
                query,
                keywords,
                max_results,
            }))
        }
        "web_fetch" => {
            only(obj, &["v", "urls", "focus"])?;
            let list = obj
                .get("urls")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_URLS)
                .ok_or_else(|| bad("urls"))?;
            let mut urls = Vec::new();
            for u in list {
                let u = u.as_str().ok_or_else(|| bad("urls"))?;
                let u = public_url(u).map_err(|_| bad("a URL is not a public http(s) URL"))?;
                if !urls.contains(&u) {
                    urls.push(u);
                }
            }
            let focus = match obj.get("focus") {
                None | Some(Value::Null) => None,
                v => Some(text_field(v, "focus", MAX_FOCUS_CHARS)?),
            };
            Ok(Op::Fetch(FetchReq { urls, focus }))
        }
        _ => Err(SafeError::new(
            "unsupported operation: this computer serves web search only",
        )),
    }
}

/// A public `http(s)` URL (no user info, no private or local host),
/// normalized with its fragment dropped. The same rule as Metro's server:
/// a private name or address would leak to a third-party provider.
pub fn public_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > MAX_URL_BYTES || raw.chars().any(char::is_control) {
        bail!("bad URL");
    }
    let mut url = url::Url::parse(raw)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("bad URL");
    }
    let private = match url.host() {
        None => true,
        Some(url::Host::Ipv4(ip)) => web_privacy::private_ipv4(ip),
        Some(url::Host::Ipv6(ip)) => web_privacy::private_ipv6(ip),
        Some(url::Host::Domain(d)) => web_privacy::private_name(&d.to_ascii_lowercase()),
    };
    if private {
        bail!("private URL");
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

// ---------------------------------------------------------------------------
// Answers and caps.

const MAX_TITLE_CHARS: usize = 300;
const MAX_SNIPPET_CHARS: usize = 1_000;
const MAX_PUBLISHED_CHARS: usize = 64;
pub const MAX_PAGE_BYTES: usize = 24 * 1024;
pub const MAX_PAGES_BYTES: usize = 60 * 1024;
pub const MAX_TEXT_BYTES: usize = 32 * 1024;
/// Largest answer sent to Metro (its limit is 256 KiB).
pub const MAX_ANSWER_BYTES: usize = 240 * 1024;
/// Largest vendor response read.
pub const MAX_VENDOR_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub published: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub final_url: Option<String>,
    pub title: Option<String>,
    pub text: String,
    pub truncated: bool,
}

/// Why one URL couldn't be read: a result, never a provider failure.
pub const FETCH_REASONS: [&str; 6] = [
    "page_not_found",
    "blocked",
    "timeout",
    "unsupported",
    "too_large",
    "failed",
];

/// What a provider returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub results: Vec<Hit>,
    pub pages: Vec<Page>,
    pub failed_urls: Vec<(String, &'static str)>,
    /// Preformatted text (the MCP providers).
    pub text: Option<String>,
}

/// Drop control characters (keeping newlines and tabs) and cut to `max`
/// bytes on a character boundary; returns whether it cut.
pub fn clean_text(s: &str, max: usize) -> (String, bool) {
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let cut = out.len() > max;
    if cut {
        let mut end = max;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
    }
    (out, cut)
}

fn one_line(s: &str, max_chars: usize) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.trim().chars().take(max_chars).collect()
}

/// The answer as Metro's wire, bounded: results re-checked (a bad URL drops
/// its result), text cut to Metro's limits.
fn answer_json(op: &Op, answer: Answer) -> Value {
    let mut out = Map::new();
    if let Some(t) = answer.text.as_deref() {
        let (t, _) = clean_text(t, MAX_TEXT_BYTES);
        if !t.trim().is_empty() {
            out.insert("text".into(), json!(t));
        }
    }
    match op {
        Op::Search(req) => {
            let results: Vec<Value> = answer
                .results
                .into_iter()
                .filter_map(|h| {
                    let url = public_url(&h.url).ok()?;
                    let mut r = json!({
                        "title": one_line(&h.title, MAX_TITLE_CHARS),
                        "url": url,
                        "snippet": one_line(&h.snippet, MAX_SNIPPET_CHARS),
                    });
                    if let Some(p) = h.published.map(|p| one_line(&p, MAX_PUBLISHED_CHARS))
                        && !p.is_empty()
                    {
                        r["published"] = json!(p);
                    }
                    Some(r)
                })
                .take(req.max_results as usize)
                .collect();
            out.insert("results".into(), Value::Array(results));
        }
        Op::Fetch(_) => {
            let mut total = 0;
            let pages: Vec<Value> = answer
                .pages
                .into_iter()
                .filter_map(|p| {
                    let url = public_url(&p.url).ok()?;
                    let room = MAX_PAGE_BYTES.min(MAX_PAGES_BYTES.saturating_sub(total));
                    let (text, cut) = clean_text(&p.text, room);
                    total += text.len();
                    let mut v = json!({"url": url, "text": text, "truncated": cut || p.truncated});
                    if let Some(f) = p.final_url.and_then(|f| public_url(&f).ok()) {
                        v["final_url"] = json!(f);
                    }
                    if let Some(t) = p.title.map(|t| one_line(&t, MAX_TITLE_CHARS))
                        && !t.is_empty()
                    {
                        v["title"] = json!(t);
                    }
                    Some(v)
                })
                .take(MAX_URLS)
                .collect();
            out.insert("pages".into(), Value::Array(pages));
            let failed: Vec<Value> = answer
                .failed_urls
                .into_iter()
                .filter_map(|(u, r)| {
                    let url = public_url(&u).ok()?;
                    Some(json!({"url": url, "reason": r}))
                })
                .take(MAX_URLS)
                .collect();
            out.insert("failed_urls".into(), Value::Array(failed));
        }
    }
    Value::Object(out)
}

/// The forms a key could come back in: as is, percent-encoded (upper and
/// lower hex, `+` or `%20` for spaces) and base64 (standard and URL-safe, at
/// each of the three byte alignments a longer encoded value could put it in).
pub(crate) fn key_forms(key: &str) -> Vec<String> {
    let mut out = encoded_forms(key);
    // A truncated echo still leaks most of the key: a long prefix counts
    // too (at least 16 characters, or three quarters of the key).
    let n = 16.max((key.len() * 3).div_ceil(4));
    if n < key.len() && key.is_char_boundary(n) {
        out.extend(encoded_forms(&key[..n]));
    }
    out.sort();
    out.dedup();
    out
}

fn encoded_forms(key: &str) -> Vec<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    let mut out = vec![key.to_owned()];
    let pct = |upper: bool| -> String {
        key.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                    (b as char).to_string()
                } else if upper {
                    format!("%{b:02X}")
                } else {
                    format!("%{b:02x}")
                }
            })
            .collect()
    };
    out.push(pct(true));
    out.push(pct(false));
    out.push(url::form_urlencoded::byte_serialize(key.as_bytes()).collect());
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for pad in 0..3usize {
            let mut bytes = vec![0u8; pad];
            bytes.extend_from_slice(key.as_bytes());
            let enc = engine.encode(&bytes);
            // Drop the characters that depend on the unknown bytes around
            // the key (leading pad bytes, a partial last group).
            let skip = (pad * 4).div_ceil(3);
            let full = ((pad + key.len()) / 3) * 4;
            if full > skip + 4 {
                out.push(enc[skip..full].to_owned());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `s` with every `%XX` decoded (any characters, over-encoded or not).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (
                (b[i + 1] as char).to_digit(16),
                (b[i + 2] as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether any string in `v` carries `key` in one of [`key_forms`], as is
/// or after percent-decoding.
pub(crate) fn reflects(v: &Value, key: &Secret) -> bool {
    let forms = key_forms(key.expose());
    fn walk(v: &Value, forms: &[String]) -> bool {
        match v {
            Value::String(s) => {
                let decoded = percent_decode(s);
                forms
                    .iter()
                    .any(|f| s.contains(f.as_str()) || decoded.contains(f.as_str()))
            }
            Value::Array(a) => a.iter().any(|x| walk(x, forms)),
            Value::Object(o) => o
                .iter()
                .any(|(k, x)| walk(&Value::String(k.clone()), forms) || walk(x, forms)),
            _ => false,
        }
    }
    walk(v, &forms)
}

// ---------------------------------------------------------------------------
// Endpoints.

/// The vendor URLs. Compiled in; only a loopback test override can replace
/// one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoints {
    pub tavily: String,
    pub exa: String,
    pub parallel: String,
    pub firecrawl: String,
    pub tinyfish_search: String,
    pub tinyfish_fetch: String,
}

pub const ENV_TEST_BASE: &str = "METRO_WEB_TEST_BASE_";

impl Endpoints {
    pub fn vendor() -> Self {
        Self {
            tavily: "https://api.tavily.com".into(),
            exa: "https://mcp.exa.ai/mcp".into(),
            parallel: "https://mcp.parallel.ai/mcp".into(),
            firecrawl: "https://api.firecrawl.dev/v2".into(),
            tinyfish_search: "https://api.search.tinyfish.ai".into(),
            tinyfish_fetch: "https://api.fetch.tinyfish.ai".into(),
        }
    }

    /// [`Self::vendor`] with `METRO_WEB_TEST_BASE_<PROVIDER>` overrides,
    /// each of which must be a loopback URL (tests only); anything else is
    /// refused.
    pub fn from_env() -> Result<Self> {
        Self::with_overrides(|k| std::env::var(k).ok())
    }

    pub fn with_overrides(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let mut e = Self::vendor();
        for p in ProviderId::ALL {
            for suffix in p.env_suffixes() {
                let key = format!("{ENV_TEST_BASE}{suffix}");
                let Some(raw) = get(&key).filter(|v| !v.trim().is_empty()) else {
                    continue;
                };
                let url = validate_base_url(&raw, &key)?;
                if !url_is_loopback(&url) {
                    bail!("{key} may only point to this machine (a test fixture)");
                }
                match *suffix {
                    "TAVILY" => e.tavily = url,
                    "EXA" => e.exa = url,
                    "PARALLEL" => e.parallel = url,
                    "FIRECRAWL" => e.firecrawl = url,
                    "TINYFISH_SEARCH" => e.tinyfish_search = url,
                    _ => e.tinyfish_fetch = url,
                }
            }
        }
        Ok(e)
    }
}

// ---------------------------------------------------------------------------
// The engine: rotation over the person's providers, per job.

pub const TURNED_OFF: &str = "web search is turned off on this computer";
pub const SETTINGS_UNREADABLE: &str = "the web search settings on this computer can't be read";
pub const KEYS_UNREADABLE: &str =
    "the saved web search keys on this computer can't be read; reset them in Settings → Web Search";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Consent {
    Yes,
    /// Web search is on, but this provider is turned off.
    ProviderOff,
    /// The master switch is off, or the settings can't be read.
    Off,
}

/// Per-attempt timeouts, always within the job's own deadline.
const SEARCH_ATTEMPT: Duration = Duration::from_secs(20);
const FETCH_ATTEMPT: Duration = Duration::from_secs(60);
/// Below this much time left, no further provider is started.
const MIN_ATTEMPT: Duration = Duration::from_millis(1500);

/// Wall-clock seconds; replaceable in tests.
pub type Clock = Arc<dyn Fn() -> u64 + Send + Sync>;

pub fn system_clock() -> Clock {
    Arc::new(time::now_secs)
}

/// One provider call's context.
pub(crate) struct Call<'a> {
    /// Consent right now: re-read before every outbound request of this
    /// attempt. `false` means stop without sending anything more.
    pub gate: &'a (dyn Fn() -> bool + Sync),
    pub http: &'a reqwest::Client,
    pub key: Option<&'a Secret>,
    pub timeout: Duration,
    pub now: u64,
    pub sessions: &'a mcp::Sessions,
}

/// Runs web jobs for one person on this computer: their settings, keys and
/// cooldowns live in `dir` (their web profile).
pub struct Engine {
    dir: PathBuf,
    endpoints: Endpoints,
    http: reqwest::Client,
    /// Keyless Exa needs a direct IPv4 connection: no proxy, IPv4 only.
    exa_direct: reqwest::Client,
    sessions: mcp::Sessions,
    /// Serializes read-modify-write of the settings file.
    write: tokio::sync::Mutex<()>,
    clock: Clock,
}

impl Engine {
    pub fn new(dir: &Path, endpoints: Endpoints) -> Result<Self> {
        Self::with_clock(dir, endpoints, system_clock())
    }

    pub fn with_clock(dir: &Path, endpoints: Endpoints, clock: Clock) -> Result<Self> {
        let base = || {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .user_agent(crate::USER_AGENT)
        };
        Ok(Self {
            dir: dir.to_path_buf(),
            endpoints,
            http: base().build()?,
            exa_direct: base()
                .no_proxy()
                .local_address(std::net::IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .build()?,
            sessions: mcp::Sessions::default(),
            write: tokio::sync::Mutex::new(()),
            clock,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn now(&self) -> u64 {
        (self.clock)()
    }

    /// One claimed job, end to end: the result body for Metro
    /// (`{ok:true,response}` or `{ok:false,error}`).
    pub async fn handle(
        &self,
        job_id: &str,
        operation: &str,
        request: &Value,
        deadline: Instant,
    ) -> Value {
        let op = match parse_job(operation, request) {
            Ok(op) => op,
            Err(e) => return json!({"ok": false, "error": e.as_str()}),
        };

        let response = match self.rotate_inner(job_id, &op, deadline).await {
            Ok(r) => r,
            Err(why) => return json!({"ok": false, "error": why}),
        };
        let len = serde_json::to_vec(&response).map_or(usize::MAX, |b| b.len());
        if len > MAX_ANSWER_BYTES {
            return json!({"ok": false, "error": "the web answer is too large to send to Metro"});
        }
        json!({"ok": true, "response": response})
    }

    fn client(&self, p: ProviderId, mode: Mode) -> &reqwest::Client {
        let real_exa = !url_is_loopback(&self.endpoints.exa);
        if p == ProviderId::Exa && mode == Mode::Keyless && real_exa {
            &self.exa_direct
        } else {
            &self.http
        }
    }

    async fn call(&self, p: ProviderId, op: &Op, c: &Call<'_>) -> Result<Answer, Failure> {
        let e = &self.endpoints;
        match (p, op) {
            (ProviderId::Tavily, Op::Search(r)) => tavily::search(c, &e.tavily, r).await,
            (ProviderId::Tavily, Op::Fetch(r)) => tavily::fetch(c, &e.tavily, r).await,
            (ProviderId::Exa, Op::Search(r)) => exa::search(c, &e.exa, r).await,
            (ProviderId::Exa, Op::Fetch(r)) => exa::fetch(c, &e.exa, r).await,
            (ProviderId::Parallel, Op::Search(r)) => parallel::search(c, &e.parallel, r).await,
            (ProviderId::Parallel, Op::Fetch(r)) => parallel::fetch(c, &e.parallel, r).await,
            (ProviderId::Firecrawl, Op::Search(r)) => firecrawl::search(c, &e.firecrawl, r).await,
            (ProviderId::Firecrawl, Op::Fetch(r)) => firecrawl::fetch(c, &e.firecrawl, r).await,
            (ProviderId::TinyFish, Op::Search(r)) => {
                tinyfish::search(c, &e.tinyfish_search, r).await
            }
            (ProviderId::TinyFish, Op::Fetch(r)) => tinyfish::fetch(c, &e.tinyfish_fetch, r).await,
        }
    }

    /// Try the person's providers in order until one answers. Always returns
    /// Metro's answer object; `provider: null` when every one failed.
    pub async fn rotate(&self, job_id: &str, op: &Op, deadline: Instant) -> Value {
        self.rotate_inner(job_id, op, deadline)
            .await
            .unwrap_or_else(|_| json!({"v": 1, "provider": null, "attempts": []}))
    }

    /// [`Self::rotate`], or the fixed reason no provider may be contacted.
    /// Consent is checked again before every attempt and every outbound
    /// request; any doubt (an unreadable file, the switch off) stops the job
    /// without contacting anyone further.
    async fn rotate_inner(
        &self,
        job_id: &str,
        op: &Op,
        deadline: Instant,
    ) -> Result<Value, &'static str> {
        let snapshot = match settings::load(&self.dir) {
            Ok(s) if s.enabled => s,
            Ok(_) => return Err(TURNED_OFF),
            Err(_) => return Err(SETTINGS_UNREADABLE),
        };
        let keys = match keys::KeyStore::new(&self.dir).load() {
            Ok(k) => k,
            Err(_) => {
                log(
                    job_id,
                    op,
                    None,
                    None,
                    "saved keys unreadable; nothing sent",
                );
                return Err(KEYS_UNREADABLE);
            }
        };
        let per_attempt = match op {
            Op::Search(_) => SEARCH_ATTEMPT,
            Op::Fetch(_) => FETCH_ATTEMPT,
        };
        let mut attempts: Vec<Value> = Vec::new();
        let mut updates = settings::Updates::default();
        let mut answer: Option<(ProviderId, Mode, Value)> = None;
        for ps in &snapshot.providers {
            if !ps.enabled {
                continue;
            }
            let p = ps.id;
            let key = keys.get(p.as_str());
            let mode = if key.is_some() {
                Mode::Key
            } else {
                Mode::Keyless
            };
            let now = self.now();
            let mut push = |outcome: Outcome, until: Option<u64>| {
                let mut a = json!({"provider": p.as_str(), "mode": mode.as_str(), "outcome": outcome.as_str()});
                if let Some(u) = until {
                    a["until"] = json!(rfc3339(u));
                }
                attempts.push(a);
            };
            if key.is_none() && !p.keyless() {
                push(Outcome::SkippedNoKey, None);
                continue;
            }
            if let Some(k) = key
                && snapshot.key_rejected(p, &keys::fingerprint(k))
            {
                push(Outcome::KeyRejected, None);
                continue;
            }
            if let Some(until) = snapshot.cooldown_until(p, mode, now) {
                push(Outcome::SkippedCooldown, Some(until));
                continue;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left < MIN_ATTEMPT {
                break;
            }
            // Right before contacting this provider: still on, and this
            // provider still allowed?
            match self.consent(Some(p)) {
                Consent::Yes => {}
                Consent::ProviderOff => continue,
                Consent::Off => return Err(TURNED_OFF),
            }
            let gate = || self.consent(Some(p)) == Consent::Yes;
            let call = Call {
                gate: &gate,
                http: self.client(p, mode),
                key,
                timeout: per_attempt.min(left),
                now,
                sessions: &self.sessions,
            };
            let result = match tokio::time::timeout(call.timeout, self.call(p, op, &call)).await {
                Ok(r) => r,
                Err(_) => Err(Failure::of(Outcome::Timeout)),
            };
            // What would leave this computer, checked for the key used on
            // this request: a vendor that echoes it back fails the attempt,
            // and nothing of that answer is delivered.
            let result = result.and_then(|a| {
                let body = answer_json(op, a);
                match key {
                    Some(k) if reflects(&body, k) => Err(Failure::of(Outcome::BadResponse)),
                    _ => Ok(body),
                }
            });
            match result {
                Ok(a) => {
                    push(Outcome::Ok, None);
                    updates.last.push((p, Outcome::Ok, now));
                    log(job_id, op, Some(p), Some(mode), "ok");
                    answer = Some((p, mode, a));
                    break;
                }
                Err(f) if f.outcome == Outcome::SkippedDisabled => {
                    // Turned off during this attempt: nothing more was sent.
                    log(job_id, op, Some(p), Some(mode), "stopped: turned off");
                    if self.consent(None) == Consent::Off {
                        self.record(job_id, op, &updates).await;
                        return Err(TURNED_OFF);
                    }
                }
                Err(f) => {
                    log(job_id, op, Some(p), Some(mode), f.outcome.as_str());
                    updates.last.push((p, f.outcome, now));
                    if f.outcome == Outcome::KeyRejected {
                        if let Some(k) = key {
                            updates.rejected.push((p, keys::fingerprint(k)));
                        }
                        push(Outcome::KeyRejected, None);
                        continue;
                    }
                    let secs = f
                        .cooldown
                        .or(f.outcome.default_cooldown())
                        .map(|s| s.clamp(1, 24 * 3600));
                    let until = secs.map(|s| now + s);
                    if let Some(u) = until {
                        updates.cooldowns.push((p, mode, u, f.outcome));
                    }
                    push(f.outcome, until);
                }
            }
        }
        self.record(job_id, op, &updates).await;
        let mut out = json!({"v": 1, "attempts": attempts});
        match answer {
            Some((p, mode, body)) => {
                if let Value::Object(fields) = body {
                    for (k, v) in fields {
                        out[k] = v;
                    }
                }
                out["provider"] = json!(p.as_str());
                out["mode"] = json!(mode.as_str());
            }
            None => out["provider"] = Value::Null,
        }
        Ok(out)
    }

    async fn record(&self, job_id: &str, op: &Op, updates: &settings::Updates) {
        let _g = self.write.lock().await;
        if let Err(e) = settings::apply(&self.dir, updates) {
            log(
                job_id,
                op,
                None,
                None,
                &format!("settings not saved: {}", clean_line(&e.to_string(), 80)),
            );
        }
    }

    /// Consent now, from the files as they are (any process may have
    /// changed them): the master switch, and `p`'s own switch.
    fn consent(&self, p: Option<ProviderId>) -> Consent {
        match settings::load(&self.dir) {
            Ok(s) if !s.enabled => Consent::Off,
            Err(_) => Consent::Off,
            Ok(s) => match p {
                Some(p) if !s.providers.iter().any(|x| x.id == p && x.enabled) => {
                    Consent::ProviderOff
                }
                _ => Consent::Yes,
            },
        }
    }

    /// One user-initiated test of `p` with a fixed query, ignoring (and not
    /// setting) cooldowns. Returns the outcome word, or the fixed reason
    /// nothing was sent (web search off, unreadable settings or keys).
    pub async fn test_provider(&self, p: ProviderId) -> Result<Outcome, &'static str> {
        match settings::load(&self.dir) {
            Ok(s) if s.enabled => {}
            Ok(_) => return Err(TURNED_OFF),
            Err(_) => return Err(SETTINGS_UNREADABLE),
        }
        let keys = keys::KeyStore::new(&self.dir)
            .load()
            .map_err(|_| KEYS_UNREADABLE)?;
        let key = keys.get(p.as_str());
        if key.is_none() && !p.keyless() {
            return Ok(Outcome::SkippedNoKey);
        }
        let mode = if key.is_some() {
            Mode::Key
        } else {
            Mode::Keyless
        };
        // An explicit test of one provider: only the master switch counts.
        let gate = || self.consent(None) == Consent::Yes;
        let op = Op::Search(SearchReq {
            query: "Metro web search test".into(),
            keywords: Vec::new(),
            max_results: 1,
        });
        let now = self.now();
        let call = Call {
            gate: &gate,
            http: self.client(p, mode),
            key,
            timeout: SEARCH_ATTEMPT,
            now,
            sessions: &self.sessions,
        };
        let outcome = match tokio::time::timeout(SEARCH_ATTEMPT, self.call(p, &op, &call)).await {
            Ok(Ok(_)) => Outcome::Ok,
            Ok(Err(f)) => f.outcome,
            Err(_) => Outcome::Timeout,
        };
        let mut updates = settings::Updates::default();
        updates.last.push((p, outcome, now));
        if outcome == Outcome::KeyRejected
            && let Some(k) = key
        {
            updates.rejected.push((p, keys::fingerprint(k)));
        }
        if outcome == Outcome::Ok {
            updates.cleared.push(p);
        }
        let _g = self.write.lock().await;
        let _ = settings::apply(&self.dir, &updates);
        Ok(outcome)
    }

    /// Serialize a settings change made elsewhere (the bridge) with jobs.
    pub async fn edit<T>(&self, f: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
        let _g = self.write.lock().await;
        f(&self.dir)
    }
}

fn log(job: &str, op: &Op, p: Option<ProviderId>, mode: Option<Mode>, what: &str) {
    let job = clean_line(job, 64);
    match (p, mode) {
        (Some(p), Some(m)) => crate::run::log(format!(
            "web job {job} {}: {} ({}) {what}",
            op.name(),
            p.as_str(),
            m.as_str()
        )),
        _ => crate::run::log(format!("web job {job} {}: {what}", op.name())),
    }
}

// ---------------------------------------------------------------------------
// Shared HTTP helpers for the adapters.

/// `Retry-After` in seconds (delay or HTTP date), capped at a day.
pub(crate) fn retry_after(h: &reqwest::header::HeaderMap, now: u64) -> Option<u64> {
    let v = h.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?.trim();
    let secs = match v.parse::<u64>() {
        Ok(s) => s,
        Err(_) => time::parse_http_date(v)?.saturating_sub(now),
    };
    Some(secs.clamp(1, 24 * 3600))
}

/// Seconds until the next 00:00 UTC (daily allowances reset then).
pub(crate) fn until_utc_midnight(now: u64) -> u64 {
    86_400 - (now % 86_400)
}

/// A transport error as an attempt outcome.
pub(crate) fn transport(e: &reqwest::Error) -> Failure {
    if e.is_timeout() {
        Failure::of(Outcome::Timeout)
    } else {
        Failure::of(Outcome::Unavailable)
    }
}

/// A vendor's successful answer as read, before any field is cut or
/// reshaped: refused when it carries the request's key (see [`reflects`]).
pub(crate) fn checked(c: &Call<'_>, v: Value) -> Result<Value, Failure> {
    match c.key {
        Some(k) if reflects(&v, k) => Err(Failure::of(Outcome::BadResponse)),
        _ => Ok(v),
    }
}

/// [`vendor_json`] for a successful answer: read, then [`checked`].
pub(crate) async fn vendor_ok(c: &Call<'_>, resp: reqwest::Response) -> Result<Value, Failure> {
    checked(c, vendor_json(resp).await?)
}

/// Read a vendor JSON body (capped); anything unreadable is `bad_response`.
pub(crate) async fn vendor_json(resp: reqwest::Response) -> Result<Value, Failure> {
    let bytes = crate::http::read_capped(resp, MAX_VENDOR_BYTES)
        .await
        .map_err(|_| Failure::of(Outcome::BadResponse))?;
    serde_json::from_slice(&bytes).map_err(|_| Failure::of(Outcome::BadResponse))
}

pub(crate) fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
}
