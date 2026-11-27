//! Runner configuration, read once from the environment and validated.
//!
//! Every invalid or ambiguous setting is an error at startup (fail closed);
//! nothing silently falls back to a weaker mode.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_SERVER_URL: &str = "http://127.0.0.1:8787";

/// The server expires a claimed job this long after the claim
/// (`Limits::run_timeout` in metro-server). Work finished later is wasted.
pub const SERVER_JOB_LEASE_SECS: u64 = 120;
/// Default deadline for `workspace.check` and for one MCP session. Leaves
/// 30s of the lease for result delivery (one full default HTTP attempt).
pub const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 90;

/// Bearer prefix of a reusable (registered, protocol 2) runner credential.
/// Any other token is a legacy dedicated credential.
pub const REUSABLE_TOKEN_PREFIX: &str = "metro_rt_v2_";

/// Which kind of credential the runner holds. Decided by the token prefix
/// only, never by anything the server sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Credential {
    /// Legacy token bound to one Space: tools operate on `METRO_WORKSPACE`
    /// itself, exactly as before.
    Dedicated,
    /// Registered runner that may serve several Spaces. `METRO_WORKSPACE` is
    /// only the writable base; each job runs in `spaces/<space_id>/` and a
    /// claim without a valid `space_id` is refused.
    Reusable,
}

impl Credential {
    pub fn from_token(token: &str) -> Self {
        if token.starts_with(REUSABLE_TOKEN_PREFIX) {
            Credential::Reusable
        } else {
            Credential::Dedicated
        }
    }
}

/// Environment variables the runner reads. Kept in one place so docs and the
/// startup log can list them.
pub mod env {
    pub const TOKEN: &str = "METRO_RUNNER_TOKEN";
    pub const SERVER_URL: &str = "METRO_SERVER_URL";
    pub const WORKSPACE: &str = "METRO_WORKSPACE";
    /// Deprecated and ignored: plain HTTP no longer needs an opt-in (see
    /// `Config::plain_http_warning`). Still accepted, with any value, so
    /// existing deployments keep starting.
    pub const ALLOW_INSECURE_HTTP: &str = "METRO_ALLOW_INSECURE_HTTP";
    pub const POLL_INTERVAL_MS: &str = "METRO_POLL_INTERVAL_MS";
    pub const HTTP_TIMEOUT_SECS: &str = "METRO_HTTP_TIMEOUT_SECS";
    pub const CHECK_ARGV: &str = "METRO_CHECK_ARGV";
    pub const CHECK_TIMEOUT_SECS: &str = "METRO_CHECK_TIMEOUT_SECS";
    pub const MCP_ARGV: &str = "METRO_MCP_ARGV";
    pub const MCP_TIMEOUT_SECS: &str = "METRO_MCP_TIMEOUT_SECS";
    pub const SANDBOX: &str = "METRO_SANDBOX";
    pub const SANDBOX_IMAGE: &str = "METRO_SANDBOX_IMAGE";
    pub const SANDBOX_MEMORY: &str = "METRO_SANDBOX_MEMORY";
    pub const SANDBOX_CPUS: &str = "METRO_SANDBOX_CPUS";
    pub const SANDBOX_PIDS: &str = "METRO_SANDBOX_PIDS";
    pub const SANDBOX_USER: &str = "METRO_SANDBOX_USER";
    pub const SANDBOX_TMPFS_SIZE: &str = "METRO_SANDBOX_TMPFS_SIZE";
    pub const DOCKER_BIN: &str = "METRO_DOCKER_BIN";
    /// `1`: this host lets a managed (script-installed) runner update
    /// itself when its manager asks in Metro. Written by
    /// `install-metro-runner-linux.sh configure --self-update on|off`.
    pub const SELF_UPDATE: &str = "METRO_RUNNER_SELF_UPDATE";
}

#[derive(Debug)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err<T>(msg: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(msg.into()))
}

/// Bearer token; never printed.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerSandbox {
    pub docker_bin: String,
    /// Pinned reference: `repo@sha256:<64 hex>` or an image ID `sha256:<64 hex>`.
    pub image: String,
    pub memory: String,
    pub cpus: String,
    pub pids: u32,
    /// Numeric non-root `uid:gid`.
    pub user: String,
    pub tmpfs_size: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sandbox {
    /// Host execution. This is a trust boundary, not a sandbox.
    None,
    Docker(DockerSandbox),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub token: Secret,
    pub credential: Credential,
    /// Base URL without trailing slash.
    pub server_url: String,
    /// Canonical absolute workspace path. For a reusable credential this is
    /// the base that holds `spaces/<space_id>/`, never a tool root itself.
    pub workspace: PathBuf,
    pub poll_interval: Duration,
    pub http_timeout: Duration,
    pub check_argv: Option<Vec<String>>,
    pub check_timeout: Duration,
    pub mcp_argv: Option<Vec<String>>,
    pub mcp_timeout: Duration,
    pub sandbox: Sandbox,
    /// `METRO_RUNNER_SELF_UPDATE`: the host's opt-in to self-update. Only
    /// effective for a managed install of a release build (see `update`).
    pub self_update: bool,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Build from an arbitrary lookup function (used by tests).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |k: &str| get(k).filter(|v| !v.is_empty());

        let token = match get(env::TOKEN) {
            Some(t) => t,
            None => return err(format!("{} is required", env::TOKEN)),
        };
        if token.chars().any(|c| c.is_whitespace() || c.is_control()) || token.len() > 4096 {
            return err(format!(
                "{} contains whitespace/control characters or is too long",
                env::TOKEN
            ));
        }

        // env::ALLOW_INSECURE_HTTP is deliberately not read: plain HTTP is
        // allowed to every host, with a warning for hosts that are neither
        // loopback nor Tailscale.
        let server_url = validate_server_url(
            get(env::SERVER_URL)
                .as_deref()
                .unwrap_or(DEFAULT_SERVER_URL),
        )?;

        // Unset: the current directory, which must not be `/`.
        let (workspace, from_cwd) = match get(env::WORKSPACE) {
            Some(w) => (PathBuf::from(w), false),
            None => match std::env::current_dir() {
                Ok(d) => (d, true),
                Err(e) => {
                    return err(format!(
                        "{} is unset and the current directory can't be read: {e}",
                        env::WORKSPACE
                    ));
                }
            },
        };
        let workspace = std::fs::canonicalize(&workspace).map_err(|e| {
            ConfigError(format!(
                "{}: cannot resolve {workspace:?}: {e}",
                env::WORKSPACE
            ))
        })?;
        if !workspace.is_dir() {
            return err(format!(
                "{}: {workspace:?} is not a directory",
                env::WORKSPACE
            ));
        }
        if from_cwd && workspace == Path::new("/") {
            return err(format!(
                "{} is unset and the current directory is /; set {} or start the runner in a \
                 project directory",
                env::WORKSPACE,
                env::WORKSPACE
            ));
        }

        let poll_interval = Duration::from_millis(parse_u64(
            get(env::POLL_INTERVAL_MS),
            env::POLL_INTERVAL_MS,
            1000,
            50,
            600_000,
        )?);
        let http_timeout = Duration::from_secs(parse_u64(
            get(env::HTTP_TIMEOUT_SECS),
            env::HTTP_TIMEOUT_SECS,
            30,
            1,
            600,
        )?);
        let check_timeout = Duration::from_secs(parse_u64(
            get(env::CHECK_TIMEOUT_SECS),
            env::CHECK_TIMEOUT_SECS,
            DEFAULT_EXEC_TIMEOUT_SECS,
            1,
            SERVER_JOB_LEASE_SECS,
        )?);
        let mcp_timeout = Duration::from_secs(parse_u64(
            get(env::MCP_TIMEOUT_SECS),
            env::MCP_TIMEOUT_SECS,
            DEFAULT_EXEC_TIMEOUT_SECS,
            1,
            SERVER_JOB_LEASE_SECS,
        )?);
        // Execution plus one result POST must fit inside the server's lease,
        // otherwise the job expires while the runner is still busy with it.
        for (key, t) in [
            (env::CHECK_TIMEOUT_SECS, check_timeout),
            (env::MCP_TIMEOUT_SECS, mcp_timeout),
        ] {
            if t.as_secs() + http_timeout.as_secs() > SERVER_JOB_LEASE_SECS {
                return err(format!(
                    "{key} ({}s) + {} ({}s) exceeds the server job lease of {SERVER_JOB_LEASE_SECS}s; \
                     lower one of them so the result can be delivered before the job expires",
                    t.as_secs(),
                    env::HTTP_TIMEOUT_SECS,
                    http_timeout.as_secs()
                ));
            }
        }

        let check_argv = get(env::CHECK_ARGV)
            .map(|v| parse_argv(&v, env::CHECK_ARGV))
            .transpose()?;
        let mcp_argv = get(env::MCP_ARGV)
            .map(|v| parse_argv(&v, env::MCP_ARGV))
            .transpose()?;

        let sandbox = match get(env::SANDBOX).as_deref().unwrap_or("none") {
            "none" => {
                for k in [
                    env::SANDBOX_IMAGE,
                    env::SANDBOX_MEMORY,
                    env::SANDBOX_CPUS,
                    env::SANDBOX_PIDS,
                    env::SANDBOX_USER,
                    env::SANDBOX_TMPFS_SIZE,
                ] {
                    if get(k).is_some() {
                        // Likely a misconfiguration: the operator set sandbox
                        // knobs but not the mode. Refuse instead of running on
                        // the host by surprise.
                        return err(format!("{k} is set but {} is not 'docker'", env::SANDBOX));
                    }
                }
                Sandbox::None
            }
            "docker" => Sandbox::Docker(parse_docker(&get, &workspace)?),
            other => {
                return err(format!(
                    "{}: unsupported value {other:?} (expected 'none' or 'docker')",
                    env::SANDBOX
                ));
            }
        };

        let credential = Credential::from_token(&token);
        if credential == Credential::Reusable && workspace == Path::new("/") {
            return err(format!(
                "{}: refusing / as the base for per-Space workspaces",
                env::WORKSPACE
            ));
        }

        Ok(Config {
            credential,
            token: Secret(token),
            server_url,
            workspace,
            poll_interval,
            http_timeout,
            check_argv,
            check_timeout,
            mcp_argv,
            mcp_timeout,
            sandbox,
            self_update: parse_bool(get(env::SELF_UPDATE).as_deref(), env::SELF_UPDATE)?,
        })
    }
}

fn parse_bool(v: Option<&str>, key: &str) -> Result<bool, ConfigError> {
    match v {
        None | Some("0") | Some("false") => Ok(false),
        Some("1") | Some("true") => Ok(true),
        Some(o) => err(format!("{key}: expected 0/1/true/false, got {o:?}")),
    }
}

fn parse_u64(
    v: Option<String>,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, ConfigError> {
    let Some(v) = v else { return Ok(default) };
    let n: u64 = v
        .trim()
        .parse()
        .map_err(|_| ConfigError(format!("{key}: not an integer: {v:?}")))?;
    if n < min || n > max {
        return err(format!("{key}: {n} out of range {min}..={max}"));
    }
    Ok(n)
}

/// Parse a JSON array of strings: a fixed executable plus arguments. Never
/// interpreted by a shell.
pub fn parse_argv(v: &str, key: &str) -> Result<Vec<String>, ConfigError> {
    let argv: Vec<String> = serde_json::from_str(v)
        .map_err(|e| ConfigError(format!("{key}: must be a JSON array of strings: {e}")))?;
    if argv.is_empty() {
        return err(format!("{key}: argv must not be empty"));
    }
    if argv[0].is_empty() {
        return err(format!("{key}: executable must not be empty"));
    }
    if argv.len() > 256 || argv.iter().any(|a| a.len() > 8192 || a.contains('\0')) {
        return err(format!("{key}: too many/too long arguments or NUL byte"));
    }
    Ok(argv)
}

fn validate_server_url(raw: &str) -> Result<String, ConfigError> {
    let url = raw.trim_end_matches('/');
    let (scheme, rest) = match url.split_once("://") {
        Some(x) => x,
        None => return err(format!("{}: missing scheme in {raw:?}", env::SERVER_URL)),
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return err(format!("{}: scheme must be http or https", env::SERVER_URL));
    }
    if rest.is_empty()
        || rest.contains(['?', '#', '@'])
        || rest.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return err(format!(
            "{}: must be scheme://host[:port][/path] without credentials, query or fragment",
            env::SERVER_URL
        ));
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return err(format!("{}: missing host", env::SERVER_URL));
    }
    Ok(format!("{scheme}://{rest}"))
}

/// The host part of a validated `scheme://authority[/path]` URL, with IPv6
/// brackets, without the port.
pub(crate) fn url_host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    if authority.starts_with('[') {
        return authority
            .split_once(']')
            .map_or(authority, |(h, _)| &authority[..h.len() + 1]);
    }
    authority.rsplit_once(':').map_or(authority, |(h, _)| h)
}

impl Config {
    /// One warning line for plain HTTP to a server that is neither loopback
    /// nor Tailscale (the runner token crosses the network unencrypted), or
    /// `None`. Names the host only, never the token.
    pub fn plain_http_warning(&self) -> Option<String> {
        let scheme = self.server_url.split_once("://").map_or("", |(s, _)| s);
        metro_url_policy::plain_http_warning(scheme, url_host(&self.server_url), "runner token")
    }
}

fn parse_docker(
    get: &impl Fn(&str) -> Option<String>,
    workspace: &Path,
) -> Result<DockerSandbox, ConfigError> {
    let image = match get(env::SANDBOX_IMAGE) {
        Some(i) => i,
        None => {
            return err(format!(
                "{}=docker requires {}",
                env::SANDBOX,
                env::SANDBOX_IMAGE
            ));
        }
    };
    if !is_pinned_image(&image) {
        return err(format!(
            "{}: must be pinned by digest (repo@sha256:<64 hex> or sha256:<64 hex>), got {image:?}",
            env::SANDBOX_IMAGE
        ));
    }

    let memory = get(env::SANDBOX_MEMORY).unwrap_or_else(|| "1g".into());
    if !is_size(&memory) {
        return err(format!(
            "{}: expected e.g. 512m or 2g, got {memory:?}",
            env::SANDBOX_MEMORY
        ));
    }
    let tmpfs_size = get(env::SANDBOX_TMPFS_SIZE).unwrap_or_else(|| "256m".into());
    if !is_size(&tmpfs_size) {
        return err(format!(
            "{}: expected e.g. 64m, got {tmpfs_size:?}",
            env::SANDBOX_TMPFS_SIZE
        ));
    }
    let cpus = get(env::SANDBOX_CPUS).unwrap_or_else(|| "1".into());
    match cpus.parse::<f64>() {
        Ok(c)
            if c > 0.0
                && c <= 1024.0
                && cpus.chars().all(|ch| ch.is_ascii_digit() || ch == '.') => {}
        _ => {
            return err(format!(
                "{}: expected a positive decimal, got {cpus:?}",
                env::SANDBOX_CPUS
            ));
        }
    }
    let pids = parse_u64(get(env::SANDBOX_PIDS), env::SANDBOX_PIDS, 256, 8, 65_536)? as u32;

    let user = get(env::SANDBOX_USER).unwrap_or_else(|| "65534:65534".into());
    let mut parts = user.split(':');
    let (uid, gid) = (parts.next(), parts.next());
    let ok_num = |s: Option<&str>| s.and_then(|s| s.parse::<u32>().ok()).filter(|n| *n != 0);
    if parts.next().is_some() || ok_num(uid).is_none() || ok_num(gid).is_none() {
        return err(format!(
            "{}: must be numeric non-root uid:gid (e.g. 65534:65534), got {user:?}",
            env::SANDBOX_USER
        ));
    }

    let docker_bin = get(env::DOCKER_BIN).unwrap_or_else(|| "docker".into());

    let ws = workspace.to_string_lossy();
    if ws.contains(',') || ws.contains('\n') || workspace.to_str().is_none() {
        return err(format!(
            "{}: workspace path {ws:?} cannot be passed safely to docker --mount (comma/newline/non-UTF-8)",
            env::WORKSPACE
        ));
    }
    if workspace == Path::new("/") {
        return err("refusing to bind-mount / as the sandbox workspace");
    }
    // Refuse if the docker daemon socket lives under the workspace: it would
    // become reachable inside the container through the bind mount.
    for sock in docker_socket_candidates(get) {
        let resolved = std::fs::canonicalize(&sock).unwrap_or(sock);
        if resolved.starts_with(workspace) {
            return err(format!(
                "workspace {workspace:?} contains the docker socket {resolved:?}; refusing to mount it into the sandbox"
            ));
        }
    }

    Ok(DockerSandbox {
        docker_bin,
        image,
        memory,
        cpus,
        pids,
        user,
        tmpfs_size,
    })
}

fn docker_socket_candidates(get: &impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let mut v = vec![
        PathBuf::from("/var/run/docker.sock"),
        PathBuf::from("/run/docker.sock"),
    ];
    if let Some(h) = get("DOCKER_HOST")
        && let Some(p) = h.strip_prefix("unix://")
    {
        v.push(PathBuf::from(p));
    }
    if let Some(x) = get("XDG_RUNTIME_DIR") {
        v.push(Path::new(&x).join("docker.sock"));
    }
    v
}

pub fn is_pinned_image(image: &str) -> bool {
    let digest = match image.rsplit_once('@') {
        Some((repo, d)) => {
            if repo.is_empty()
                || !repo
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':'))
            {
                return false;
            }
            d
        }
        None => image,
    };
    match digest.strip_prefix("sha256:") {
        Some(hex) => hex.len() == 64 && hex.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        None => false,
    }
}

fn is_size(s: &str) -> bool {
    let digits = s.trim_end_matches(['b', 'k', 'm', 'g', 'B', 'K', 'M', 'G']);
    let suffix_len = s.len() - digits.len();
    !digits.is_empty()
        && suffix_len <= 1
        && digits.chars().all(|c| c.is_ascii_digit())
        && digits != "0"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const DIGEST: &str = "sha256:6ba19c50b56777f5c11c58eff8ddb69d0d5f27ba131cffb63f180ccfd43c66af";

    fn cfg(pairs: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let ws = tempfile::tempdir().unwrap();
        let mut m: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        m.entry(env::TOKEN.into()).or_insert_with(|| "tok".into());
        m.entry(env::WORKSPACE.into())
            .or_insert_with(|| ws.path().to_string_lossy().into_owned());
        Config::from_lookup(|k| m.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.server_url, DEFAULT_SERVER_URL);
        assert_eq!(c.sandbox, Sandbox::None);
        assert!(c.check_argv.is_none() && c.mcp_argv.is_none());
        assert!(
            !format!("{c:?}").contains("tok\""),
            "token must be redacted in Debug"
        );
    }

    #[test]
    fn token_and_workspace_required() {
        let m: HashMap<&str, &str> = HashMap::new();
        assert!(Config::from_lookup(|k| m.get(k).map(|s| s.to_string())).is_err());
        assert!(cfg(&[(env::WORKSPACE, "/definitely/not/here")]).is_err());
        assert!(cfg(&[(env::TOKEN, "has space")]).is_err());
    }

    #[test]
    fn server_url_rules() {
        assert!(cfg(&[(env::SERVER_URL, "http://localhost:1/")]).is_ok());
        assert!(cfg(&[(env::SERVER_URL, "http://[::1]:8787")]).is_ok());
        assert!(cfg(&[(env::SERVER_URL, "https://metro.example.com")]).is_ok());
        // Plain HTTP works to every host; no opt-in needed.
        assert!(cfg(&[(env::SERVER_URL, "http://metro.example.com")]).is_ok());
        assert!(cfg(&[(env::SERVER_URL, "http://192.168.1.5:8787")]).is_ok());
        // The old opt-in is still accepted (and ignored), whatever its value.
        for v in ["1", "0", "true", "anything"] {
            assert!(
                cfg(&[
                    (env::SERVER_URL, "http://metro.example.com"),
                    (env::ALLOW_INSECURE_HTTP, v)
                ])
                .is_ok(),
                "{v}"
            );
        }
        assert!(cfg(&[(env::SERVER_URL, "http://u:p@192.168.1.5")]).is_err());
        assert!(cfg(&[(env::SERVER_URL, "http://192.168.1.5/#x")]).is_err());
        assert!(cfg(&[(env::SERVER_URL, "https://u:p@h")]).is_err());
        assert!(cfg(&[(env::SERVER_URL, "ftp://h")]).is_err());
        assert!(cfg(&[(env::SERVER_URL, "https://h/?x=1")]).is_err());
    }

    #[test]
    fn plain_http_warning_only_outside_loopback_and_tailscale() {
        let warn = |url: &str| {
            cfg(&[
                (env::SERVER_URL, url),
                (env::TOKEN, "metro_rt_v2_secret-value"),
            ])
            .unwrap()
            .plain_http_warning()
        };
        for quiet in [
            "http://127.0.0.1:8787",
            "http://localhost:8787/",
            "http://[::1]:8787",
            "http://100.101.102.103:8787",
            "http://[fd7a:115c:a1e0::1]:8787/metro",
            "http://metro.tail1234.ts.net",
            "https://192.168.1.5",
            "https://metro.example.com",
        ] {
            assert_eq!(warn(quiet), None, "{quiet}");
        }
        for loud in [
            // A trailing dot makes it a name for the resolver, not the address.
            "http://100.64.0.1.:8787",
            "http://127.0.0.1.:8787",
            "http://localhost.:8787",
            "http://192.168.1.5:8787",
            "http://metro.example.com/base",
            "http://100.128.0.1",
            "http://[fd7a:115c:a1e1::1]:8787",
            "http://ts.net",
        ] {
            let w = warn(loud).unwrap_or_else(|| panic!("{loud}"));
            assert!(
                w.contains("isn't encrypted") && w.contains("runner token"),
                "{w}"
            );
            assert!(
                !w.contains("secret-value"),
                "the warning must never contain the token"
            );
        }
        assert!(
            warn("http://192.168.1.5:8787")
                .unwrap()
                .contains("192.168.1.5")
        );
        assert!(
            warn("http://[fd7a:115c:a1e1::1]:8787")
                .unwrap()
                .contains("[fd7a:115c:a1e1::1]")
        );
    }

    #[test]
    fn exec_deadlines_fit_server_lease() {
        let c = cfg(&[]).unwrap();
        assert_eq!(c.check_timeout, Duration::from_secs(90));
        assert_eq!(c.mcp_timeout, Duration::from_secs(90));
        assert_eq!(c.http_timeout, Duration::from_secs(30));
        // Shorter is always fine.
        assert!(
            cfg(&[
                (env::CHECK_TIMEOUT_SECS, "30"),
                (env::MCP_TIMEOUT_SECS, "45")
            ])
            .is_ok()
        );
        // Longer only if the HTTP timeout shrinks to keep the sum within the lease.
        assert!(cfg(&[(env::CHECK_TIMEOUT_SECS, "91")]).is_err());
        assert!(cfg(&[(env::MCP_TIMEOUT_SECS, "100")]).is_err());
        assert!(
            cfg(&[
                (env::CHECK_TIMEOUT_SECS, "110"),
                (env::HTTP_TIMEOUT_SECS, "10")
            ])
            .is_ok()
        );
        assert!(
            cfg(&[
                (env::CHECK_TIMEOUT_SECS, "600"),
                (env::HTTP_TIMEOUT_SECS, "1")
            ])
            .is_err()
        );
        assert!(cfg(&[(env::HTTP_TIMEOUT_SECS, "31")]).is_err());
    }

    #[test]
    fn argv_parsing() {
        let c = cfg(&[(env::CHECK_ARGV, r#"["cargo","check"]"#)]).unwrap();
        assert_eq!(c.check_argv.unwrap(), vec!["cargo", "check"]);
        assert!(cfg(&[(env::CHECK_ARGV, "cargo check")]).is_err());
        assert!(cfg(&[(env::CHECK_ARGV, "[]")]).is_err());
        assert!(cfg(&[(env::MCP_ARGV, r#"[1,2]"#)]).is_err());
    }

    #[test]
    fn sandbox_is_opt_in_and_strict() {
        // Unknown mode refused.
        assert!(cfg(&[(env::SANDBOX, "podman")]).is_err());
        // Docker without image refused (no host fallback).
        assert!(cfg(&[(env::SANDBOX, "docker")]).is_err());
        // Unpinned tag refused.
        assert!(cfg(&[(env::SANDBOX, "docker"), (env::SANDBOX_IMAGE, "alpine:3")]).is_err());
        // Root user refused.
        let img = format!("redis@{DIGEST}");
        assert!(
            cfg(&[
                (env::SANDBOX, "docker"),
                (env::SANDBOX_IMAGE, &img),
                (env::SANDBOX_USER, "0:0")
            ])
            .is_err()
        );
        assert!(
            cfg(&[
                (env::SANDBOX, "docker"),
                (env::SANDBOX_IMAGE, &img),
                (env::SANDBOX_USER, "root")
            ])
            .is_err()
        );
        // Sandbox knobs without docker mode refused.
        assert!(cfg(&[(env::SANDBOX_IMAGE, &img)]).is_err());
        let c = cfg(&[(env::SANDBOX, "docker"), (env::SANDBOX_IMAGE, &img)]).unwrap();
        match c.sandbox {
            Sandbox::Docker(d) => {
                assert_eq!(d.user, "65534:65534");
                assert_eq!(d.pids, 256);
            }
            Sandbox::None => panic!("expected docker"),
        }
    }

    #[test]
    fn credential_kind_comes_from_token_prefix() {
        assert_eq!(cfg(&[]).unwrap().credential, Credential::Dedicated);
        let c = cfg(&[(env::TOKEN, "metro_rt_v2_abc")]).unwrap();
        assert_eq!(c.credential, Credential::Reusable);
        assert!(!format!("{c:?}").contains("metro_rt_v2_abc"));
        // Similar-looking prefixes stay dedicated.
        for t in ["metro_rt_v1_abc", "xmetro_rt_v2_abc", "METRO_RT_V2_abc"] {
            assert_eq!(
                cfg(&[(env::TOKEN, t)]).unwrap().credential,
                Credential::Dedicated,
                "{t}"
            );
        }
    }

    #[test]
    fn pinned_images() {
        assert!(is_pinned_image(&format!("ghcr.io/x/y@{DIGEST}")));
        assert!(is_pinned_image(&format!("localhost:5000/y:tag@{DIGEST}")));
        assert!(is_pinned_image(DIGEST));
        assert!(!is_pinned_image("ubuntu:24.04"));
        assert!(!is_pinned_image("ubuntu@sha256:abc"));
        assert!(!is_pinned_image(&format!("@{DIGEST}")));
    }
}
