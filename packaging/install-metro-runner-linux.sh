#!/bin/sh
# Metro runner for Linux x64 (operator-managed, outbound only): per-user
# install, update, configuration and uninstall from a prebuilt
# Metro-runner-<version>-linux-x64.tar.gz. This script never creates, rotates
# or prints a runner token: you create one in Metro (Manage Space or your
# runner settings), save it to a mode-600 file and give this script its path.
set -eu
umask 022

PROG=install-metro-runner-linux
MARK=.metro-runner-install
UNIT=metro-runner.service
UNIT_TAG='# Managed by install-metro-runner-linux'

die() { printf '%s: %s\n' "$PROG" "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
    cat <<'EOF'
Usage:
  install-metro-runner-linux.sh install   --archive FILE [--sha256 HEX | --checksum-file FILE]
  install-metro-runner-linux.sh update    --archive FILE [--sha256 HEX | --checksum-file FILE]
  install-metro-runner-linux.sh configure --server-url URL --workspace DIR --token-file FILE [--self-update on|off]
  install-metro-runner-linux.sh run         (runs the runner in the foreground)
  install-metro-runner-linux.sh install-service   (writes a systemd --user unit; does not start it)
  install-metro-runner-linux.sh doctor
  install-metro-runner-linux.sh status
  install-metro-runner-linux.sh uninstall [--purge-config]

--server-url is https://, or http:// to any host. On this machine use
http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT (a stable
port). Plain http to a host other than this machine or a Tailscale address
(100.64.0.0/10, fd7a:115c:a1e0::/48, *.ts.net) works, with a warning: the
runner token crosses the network unencrypted.
--workspace is an existing directory owned by you; the runner keeps each
Space's files in spaces/<space-id>/ inside it. It is never deleted.
--token-file is a regular file owned by you with mode 600 holding the runner
token from Metro. Only its path is stored; the token is read when the runner
starts.
--self-update on lets the runner update itself when the runner's manager
clicks Update in Metro: it downloads the release from GitHub and checks its
Sigstore signature and checksums itself before switching. The Metro server
only says which version; it never sends a download link or code. New
configurations default to on; reconfiguring keeps the earlier choice (off
when there was none).

Uninstall keeps your configuration (unless --purge-config), the workspace and
the token file.
To migrate an existing installation for managed updates, stop the runner
and use this installer with update --migrate-update-lock. Only use current
installer copies afterward; old configure and uninstall did not lock.
Environment: METRO_RUNNER_INSTALL_DIR overrides the install directory
(default: ${XDG_DATA_HOME:-~/.local/share}/metro-runner).
EOF
}

absolute() {
    case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$(pwd -P)" "$1" ;; esac
}
xdg_dir() { case "${1:-}" in /*) printf '%s' "$1" ;; *) printf '%s' "$2" ;; esac; }

case "${HOME:-}" in /*) ;; *) die "HOME must be set to an absolute path" ;; esac
data_home=$(xdg_dir "${XDG_DATA_HOME:-}" "$HOME/.local/share")
config_home=$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")
root=$(xdg_dir "${METRO_RUNNER_INSTALL_DIR:-}" "$data_home/metro-runner")
config_dir=$config_home/metro-runner
config=$config_dir/runner.env
unit_file=$config_home/systemd/user/$UNIT

# Same open-directory flock as metro-runner. Closing fd 9 releases only
# this invocation's lock; there is no stale lock file to delete.
lock_install() {
    [ -d "$root" ] && [ ! -L "$root" ] || die "install directory is not a plain directory"
    [ "$(stat -c '%u' -- "$root")" = "$(id -u)" ] || die "install directory is not owned by you"
    mode=$(stat -c '%a' -- "$root")
    [ "$((0$mode & 022))" -eq 0 ] || die "install directory is writable by others"
    exec 9<"$root"
    flock -n -x 9 || die "another install is running"
    [ ! -L "$root" ] && [ "$root" -ef /proc/self/fd/9 ] || die "the install directory changed"
    compat_take
}

compat_retire() {
    compat_tomb=$(mktemp -d -- "$root/.unlock.XXXXXX") || return 1
    rmdir -- "$compat_tomb" || return 1
    mv -nT -- "$root/.lock" "$compat_tomb" || return 1
    [ -f "$compat_tomb/owner.metro-lock-v2" ] || return 1
    rm -- "$compat_tomb/owner.metro-lock-v2" && rmdir -- "$compat_tomb"
}

compat_take() {
    compat_owner=$root/.lock/owner.metro-lock-v2
    if { [ -e "$root/.lock" ] || [ -L "$root/.lock" ]; } &&
        ! { [ ! -L "$root/.lock" ] && [ ! -L "$compat_owner" ] && [ -f "$compat_owner" ] &&
            [ "$(ls -A -- "$root/.lock")" = owner.metro-lock-v2 ] &&
            [ "$(stat -c '%s' -- "$compat_owner")" -le 128 ] &&
            [ "$(wc -l <"$compat_owner")" -eq 1 ] &&
            [ "$(LC_ALL=C grep -ac '^' "$compat_owner")" -eq 1 ] &&
            LC_ALL=C grep -aExq 'metro-lock-v2:[A-Za-z0-9._-]+' "$compat_owner" &&
            compat_retire; }; then
        die "another install holds the legacy lock at $root/.lock"
    fi
    compat_new=$(mktemp -d -- "$root/.lock-new.XXXXXX")
    compat_token=metro-lock-v2:${compat_new##*/}
    printf '%s\n' "$compat_token" >"$compat_new/owner.metro-lock-v2"
    mv -nT -- "$compat_new" "$root/.lock"
    if [ -d "$compat_new" ]; then
        rm -- "$compat_new/owner.metro-lock-v2"
        rmdir -- "$compat_new"
        die "another install holds the legacy lock at $root/.lock"
    fi
    trap 'compat_release' EXIT
}

compat_release() {
    if [ "$root" -ef /proc/self/fd/9 ] && [ ! -L "$root/.lock" ] &&
        [ ! -L "$compat_owner" ] && [ -f "$compat_owner" ] &&
        [ "$(cat -- "$compat_owner")" = "$compat_token" ]; then
        compat_retire || :
    fi
}

no_pending_update() {
    if [ -e "$root/.update-pending.json" ] || [ -L "$root/.update-pending.json" ]; then
        die "a runner update journal is present; finish recovery before changing the install"
    fi
}

action=${1:-}
[ $# -gt 0 ] && shift
archive='' sha='' checksum_file='' server_url='' workspace='' token_file='' purge=0 self_update='' migrate=0
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) [ $# -ge 2 ] || die "--archive needs a file"; archive=$2; shift 2 ;;
        --sha256) [ $# -ge 2 ] || die "--sha256 needs a value"; sha=$2; shift 2 ;;
        --checksum-file) [ $# -ge 2 ] || die "--checksum-file needs a file"; checksum_file=$2; shift 2 ;;
        --server-url) [ $# -ge 2 ] || die "--server-url needs a URL"; server_url=$2; shift 2 ;;
        --workspace) [ $# -ge 2 ] || die "--workspace needs a directory"; workspace=$2; shift 2 ;;
        --token-file) [ $# -ge 2 ] || die "--token-file needs a file"; token_file=$2; shift 2 ;;
        --purge-config) purge=1; shift ;;
        --migrate-update-lock) migrate=1; shift ;;
        --self-update)
            [ $# -ge 2 ] || die "--self-update needs on or off"
            case "$2" in on|off) self_update=$2 ;; *) die "--self-update must be on or off" ;; esac
            shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done
[ "$migrate" = 0 ] || [ "$action" = update ] || die "--migrate-update-lock requires update"

# ---------------------------------------------------------------- archive checks
sha_of() { sha256sum -- "$1" | cut -d' ' -f1; }

lookup_sum() {
    awk -v n="$2" '($2 == n || $2 == "*" n) && $1 ~ /^[0-9a-fA-F]+$/ && length($1) == 64 { print tolower($1); f++ }
                   END { exit f == 1 ? 0 : 1 }' "$1"
}

verify_archive_checksum() {
    name=${1##*/}
    if [ -n "$sha" ]; then
        expected=$(printf '%s' "$sha" | tr 'A-F' 'a-f'); source="--sha256"
    else
        if [ -z "$checksum_file" ]; then
            if [ -f "$1.sha256" ]; then checksum_file=$1.sha256
            elif [ -f "${1%/*}/SHA256SUMS" ]; then checksum_file=${1%/*}/SHA256SUMS
            else die "no checksum for $name: pass --sha256 HEX or --checksum-file FILE (the release's $name.sha256 or SHA256SUMS)"
            fi
        fi
        [ -f "$checksum_file" ] || die "checksum file not found: $checksum_file"
        expected=$(lookup_sum "$checksum_file" "$name") || die "$checksum_file does not list exactly one SHA-256 for $name"
        source=$checksum_file
    fi
    case "$expected" in *[!0-9a-f]*|'') die "invalid SHA-256 value from $source" ;; esac
    [ ${#expected} -eq 64 ] || die "invalid SHA-256 value from $source"
    actual=$(sha_of "$1")
    [ "$actual" = "$expected" ] || die "checksum mismatch for $name (expected $expected, got $actual). Download it again; do not install it."
    say "Checksum OK: $name"
}

manifest_sums() {
    awk '
        /^  "files": \{$/ { inside = 1; next }
        inside && /^  \},?$/ { inside = 0; closed = 1; next }
        inside {
            line = $0
            if (line !~ /^    "[^"\\]+": "[0-9a-f]+",?$/) { bad = 1; next }
            sub(/^    "/, "", line); sub(/",?$/, "", line)
            i = index(line, "\": \""); path = substr(line, 1, i - 1); hash = substr(line, i + 4)
            if (length(hash) != 64 || path ~ /^\// || path ~ /(^|\/)\.\.?(\/|$)/ || path == "RELEASE.json") { bad = 1; next }
            print hash "  " path; n++
        }
        END { exit (bad || !closed || n == 0) ? 1 : 0 }' "$1"
}

json_field() {
    sed -n "s/^  \"$2\": \"\\([^\"\\\\]*\\)\",\\{0,1\\}\$/\\1/p" "$1"
}

extract_verified() {
    listing=$2/.list
    tar -tzvf "$1" >"$listing" || die "cannot read $1"
    if awk '{ if (substr($1, 1, 1) != "-") bad = 1 } END { exit bad ? 0 : 1 }' "$listing"; then
        die "archive contains something other than regular files; refusing it"
    fi
    tar -tzf "$1" >"$listing"
    awk -v top="$3/" 'index($0, top) != 1 || $0 ~ /(^|\/)\.\.(\/|$)/ { bad = 1 } END { exit bad ? 0 : 1 }' "$listing" &&
        die "archive entries must all be inside $3/; refusing it"
    rm -f -- "$listing"
    tar -xzf "$1" -C "$2" --no-same-owner || die "cannot extract $1"
    dir=$2/$3
    [ -f "$dir/RELEASE.json" ] || die "archive has no RELEASE.json manifest"
    manifest_sums "$dir/RELEASE.json" >"$2/.sums" || die "RELEASE.json has no valid file list"
    (cd "$dir" && sha256sum --check --strict --quiet -- "$2/.sums") >/dev/null 2>&1 ||
        die "files in the archive do not match its RELEASE.json manifest"
    (cd "$dir" && find . ! -type d ! -type f | grep -q .) && die "archive contains links or special files"
    { cut -c67- "$2/.sums"; echo RELEASE.json; } | LC_ALL=C sort >"$2/.want"
    (cd "$dir" && find . -type f | sed 's|^\./||' | LC_ALL=C sort) >"$2/.have"
    [ "$(cat -- "$2/.want")" = "$(cat -- "$2/.have")" ] || die "archive has files that its RELEASE.json does not list"
    rm -f -- "$2/.sums" "$2/.want" "$2/.have"
}

# ---------------------------------------------------------------- host checks
version_at_least() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; }

host_problems() { # $1 = runner binary (optional); prints one problem per line
    [ "$(uname -s)" = Linux ] || echo "the native runner needs Linux; on macOS or Windows use metro-runner-container"
    case "$(uname -m)" in x86_64|amd64) ;; *) echo "the native runner is built for x86_64 only (this machine is $(uname -m))" ;; esac
    kernel=$(uname -r | sed 's/[^0-9.].*//')
    version_at_least "$kernel" 5.6 || echo "Linux kernel 5.6 or newer is required for openat2 (this is $(uname -r))"
    glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc //p')
    if [ -z "$glibc" ]; then echo "glibc 2.36 or newer is required (none found; musl systems are not supported)"
    elif ! version_at_least "$glibc" 2.36; then echo "glibc 2.36 or newer is required (this is $glibc)"; fi
    if [ -n "${1:-}" ] && [ -f "$1" ]; then
        ldd "$1" 2>/dev/null | awk '/=> not found/ { print "missing library: " $1 }'
    fi
    return 0
}

# ---------------------------------------------------------------- config
# url-rule:begin (test_installers.py runs this block against
# crates/url-policy/testdata/host_vectors.json)
# Plain http works to any host. The host is parsed for real: an IPv6 address
# in brackets, a canonical dotted-quad IPv4 address, or a DNS name (labels of
# letters, digits and inner hyphens; one trailing dot allowed). Then:
# - loopback: only the exact literals localhost, 127.0.0.1 or [::1] with an
#   explicit port 1-65535; any other spelling of this machine (127.1,
#   LOCALHOST, 127.0.0.2, [0:0:0:0:0:0:0:1], ::ffff: forms, 0.0.0.0, [::]) is
#   refused as ambiguous;
# - Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48, names ending in .ts.net):
#   encrypted by Tailscale itself, used silently, and always reached without a
#   proxy (see direct_no_proxy);
# - any other host: allowed, with one warning that the runner token crosses
#   the network unencrypted.
# Numbers must be a canonical dotted quad: resolvers read 100.077.0.1 as
# octal, 0x64 as hex and 100.64.1 as shorthand, so those are refused.
# Its own variables all start with _hc_ (POSIX sh has no locals).
# http_class URL sets url_class (loopback, tailscale or other), url_host (as
# written, IPv6 in brackets) and url_port (empty when absent); it fails on a
# malformed or ambiguous authority. After an IPv6 literal's `]` only an
# optional `:<digits>` may follow.
is_label() {
    case "$1" in ''|-*|*-|*[!A-Za-z0-9-]*) return 1 ;; esac
    [ ${#1} -le 63 ]
}

v6_part() { # colon-separated groups of 1-4 hex digits (or nothing): prints them padded
    [ -n "$1" ] || return 0
    case "$1" in :*|*:) return 1 ;; esac
    (
        IFS=:
        # shellcheck disable=SC2086 # split on colons on purpose
        set -- $1
        for _hc_g in "$@"; do
            case "$_hc_g" in ''|?????*) exit 1 ;; esac
            printf '%04x ' "0x$_hc_g"
        done
    )
}

ipv6_groups() { # prints the 8 groups, lowercase and padded, or fails
    _hc_t=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
    case "$_hc_t" in ''|*[!0-9a-f:]*|*::*::*) return 1 ;; esac
    case "$_hc_t" in
        *::*) _hc_head=${_hc_t%%::*}; _hc_tail=${_hc_t#*::}; _hc_compress=1 ;;
        *) _hc_head=$_hc_t; _hc_tail=''; _hc_compress=0 ;;
    esac
    _hc_hg=$(v6_part "$_hc_head") || return 1
    _hc_tg=$(v6_part "$_hc_tail") || return 1
    # shellcheck disable=SC2086
    _hc_n=$(( $(set -- $_hc_hg; echo $#) + $(set -- $_hc_tg; echo $#) ))
    if [ "$_hc_compress" = 1 ]; then [ "$_hc_n" -le 7 ] || return 1; else [ "$_hc_n" -eq 8 ] || return 1; fi
    _hc_zeros=''; _hc_i=$_hc_n
    while [ "$_hc_i" -lt 8 ]; do _hc_zeros="$_hc_zeros 0000"; _hc_i=$((_hc_i + 1)); done
    # shellcheck disable=SC2086
    echo $_hc_hg $_hc_zeros $_hc_tg
}

http_class() {
    _hc_rest=${1#http://}; _hc_auth=${_hc_rest%%/*}; _hc_port='' _hc_has_port=0 url_class='' url_host='' url_port=''
    case "$_hc_auth" in
        \[*)
            _hc_inner=${_hc_auth#\[}
            case "$_hc_inner" in *\]*) ;; *) return 1 ;; esac
            _hc_after=${_hc_inner#*\]}; _hc_inner=${_hc_inner%%\]*}
            url_host="[$_hc_inner]"
            case "$_hc_after" in
                '') ;;
                :*) _hc_port=${_hc_after#:}; _hc_has_port=1 ;;
                *) return 1 ;;
            esac ;;
        *\[*|*\]*|*:*:*) return 1 ;;
        *:*) url_host=${_hc_auth%:*}; _hc_port=${_hc_auth##*:}; _hc_has_port=1 ;;
        *) url_host=$_hc_auth ;;
    esac
    if [ "$_hc_has_port" = 1 ]; then
        case "$_hc_port" in ''|*[!0-9]*) return 1 ;; esac
        [ ${#_hc_port} -le 5 ] && [ "$_hc_port" -ge 1 ] && [ "$_hc_port" -le 65535 ] || return 1
        # shellcheck disable=SC2034 # read by the container script's probe
        url_port=$_hc_port
    fi
    case "$url_host" in
        \[*\])
            _hc_inner=${url_host#\[}; _hc_inner=${_hc_inner%\]}
            _hc_groups=$(ipv6_groups "$_hc_inner") || return 1
            # shellcheck disable=SC2086
            set -- $_hc_groups
            case "$*" in
                "0000 0000 0000 0000 0000 0000 0000 0000") return 1 ;;
                "0000 0000 0000 0000 0000 0000 0000 0001")
                    [ "$url_host" = '[::1]' ] && [ "$_hc_has_port" = 1 ] || return 1
                    url_class=loopback; return 0 ;;
                "0000 0000 0000 0000 0000 ffff "*) return 1 ;;
                "fd7a 115c a1e0 "*) url_class=tailscale; return 0 ;;
            esac
            url_class=other; return 0 ;;
    esac
    _hc_name=$url_host; _hc_dotted=0
    case "$_hc_name" in *.) _hc_name=${_hc_name%.}; _hc_dotted=1 ;; esac
    [ -n "$_hc_name" ] && [ ${#_hc_name} -le 253 ] || return 1
    # DNS characters only, checked before any splitting (no glob characters
    # can reach the shell), and no empty label: box.. is refused.
    case "$_hc_name" in *[!A-Za-z0-9.-]*|.*|*..*|*.) return 1 ;; esac
    _hc_labels=$(printf '%s' "$_hc_name" | tr '.' ' ')
    _hc_count=0 _hc_last=''
    set -f
    for _hc_l in $_hc_labels; do is_label "$_hc_l" || { set +f; return 1; }; _hc_count=$((_hc_count + 1)); _hc_last=$_hc_l; done
    set +f
    case "$_hc_last" in
        *[!0-9]*) case "$_hc_last" in 0[xX]*) _hc_numeric=1 ;; *) _hc_numeric=0 ;; esac ;;
        *) _hc_numeric=1 ;;
    esac
    if [ "$_hc_numeric" = 1 ]; then
        [ "$_hc_dotted" = 0 ] && [ "$_hc_count" = 4 ] || return 1
        for _hc_o in $_hc_labels; do
            case "$_hc_o" in 0|[1-9]|[1-9][0-9]|[1-9][0-9][0-9]) ;; *) return 1 ;; esac
            [ "$_hc_o" -le 255 ] || return 1
        done
        # shellcheck disable=SC2086 # four validated numeric labels
        set -- $_hc_labels
        if [ "$1" = 127 ]; then
            [ "$_hc_name" = 127.0.0.1 ] && [ "$_hc_has_port" = 1 ] || return 1
            url_class=loopback; return 0
        fi
        [ "$_hc_name" != 0.0.0.0 ] || return 1
        if [ "$1" = 100 ] && [ "$2" -ge 64 ] && [ "$2" -le 127 ]; then url_class=tailscale; return 0; fi
        url_class=other; return 0
    fi
    _hc_lower=$(printf '%s' "$_hc_name" | tr '[:upper:]' '[:lower:]')
    if [ "$_hc_lower" = localhost ] && [ "$_hc_dotted" = 0 ]; then
        [ "$url_host" = localhost ] && [ "$_hc_has_port" = 1 ] || return 1
        url_class=loopback; return 0
    fi
    case "$_hc_lower" in
        *.ts.net) [ "$_hc_count" -ge 3 ] && url_class=tailscale && return 0 ;;
    esac
    url_class=other
}
# url-rule:end

# NO_PROXY for a loopback or Tailscale server: the runner must reach it
# directly, or the "encrypted"/"local" assumption behind skipping the warning
# is false. Runners up to 0.4.0 (ureq 3.5.2) read HTTP_PROXY and friends and
# skip them only for NO_PROXY matches, compared against the URI host as
# written (IPv6 in brackets). Prints the person's NO_PROXY (or no_proxy) with
# this server's host added; other destinations keep their proxy settings.
direct_no_proxy() { # $1 = url_host
    _dnp_base=${NO_PROXY-${no_proxy-}}
    _dnp_extra=$1
    case "$1" in \[*\]) _dnp_extra="$1,${1#\[}"; _dnp_extra=${_dnp_extra%\]} ;; esac
    printf '%s' "${_dnp_base:+$_dnp_base,}$_dnp_extra"
}

plain_http_warning() { # $1 = host; never mentions the token's value
    printf '%s: warning: plain HTTP to %s isn'"'"'t encrypted: anyone on the network between this machine and %s could read the runner token. Use it only on networks you trust, such as Tailscale or a VPN, or use https://.\n' "$PROG" "$1" "$1" >&2
}

check_url() { # sets url_class (https, loopback, tailscale or other)
    u=$1
    case "$u" in *[[:space:]]*|*\?*|*\#*|*@*|*\"*|*\\*|*\$*|*\`*) die "--server-url must be a plain URL without spaces, credentials, query or fragment" ;; esac
    case "$u" in
        https://[!/:]*) url_class=https ;;
        http://?*) http_class "$u" ||
            die "invalid --server-url $u: for this machine use exactly http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT; otherwise host[:port] with a port of 1-65535" ;;
        *) die "--server-url must start with https:// or http://" ;;
    esac
}

check_token_file() { # never reads the token
    f=$1
    case "$f" in /*) ;; *) die "token file path must be absolute: $f" ;; esac
    if [ -L "$f" ] || [ ! -f "$f" ]; then
        die "token file must be a regular file (not a link): $f"
    fi
    [ -s "$f" ] || die "token file is empty: $f"
    [ "$(stat -c '%u:%a' -- "$f")" = "$(id -u):600" ] ||
        die "token file must be owned by you with mode 600 (run: chmod 600 '$f')"
}

check_plain_value() { # values go into env and unit files unquoted-safe
    case "$2" in
        *'"'*|*\\*|*\$*|*\`*|*%*) die "$1 must not contain \" \\ \$ \` or %: $2" ;;
    esac
    case "$2" in *"
"*) die "$1 must not contain a newline" ;; esac
}

# Reads runner.env: KEY="value" lines for a fixed set of keys, no shell eval.
ALLOWED_KEYS='METRO_SERVER_URL METRO_WORKSPACE METRO_RUNNER_TOKEN_FILE METRO_POLL_INTERVAL_MS METRO_HTTP_TIMEOUT_SECS METRO_CHECK_ARGV METRO_CHECK_TIMEOUT_SECS METRO_MCP_ARGV METRO_MCP_TIMEOUT_SECS METRO_SANDBOX METRO_SANDBOX_IMAGE METRO_SANDBOX_MEMORY METRO_SANDBOX_CPUS METRO_SANDBOX_PIDS METRO_SANDBOX_USER METRO_SANDBOX_TMPFS_SIZE METRO_DOCKER_BIN METRO_RUNNER_SELF_UPDATE'

load_config() {
    [ -f "$config" ] || die "not configured: run '$PROG configure --server-url URL --workspace DIR --token-file FILE'"
    [ "$(stat -c '%u:%a' -- "$config")" = "$(id -u):600" ] || die "$config must be owned by you with mode 600"
    cfg_env=''
    cfg_METRO_SERVER_URL='' cfg_METRO_WORKSPACE='' cfg_METRO_RUNNER_TOKEN_FILE=''
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in ''|'#'*) continue ;; esac
        key=${line%%=*}; value=${line#*=}
        case " $ALLOWED_KEYS " in *" $key "*) ;; *) die "$config: unsupported setting $key (tokens are never stored here)" ;; esac
        case "$value" in \"*\") value=${value#\"}; value=${value%\"} ;; esac
        eval "cfg_$key=\$value"
        cfg_env="$cfg_env $key"
    done <"$config"
    for k in METRO_SERVER_URL METRO_WORKSPACE METRO_RUNNER_TOKEN_FILE; do
        eval "[ -n \"\${cfg_$k:-}\" ]" || die "$config: $k is not set"
    done
}

keep_script_copy() { # this script, beside the install (`run` and the unit use it)
    self=$(absolute "$0")
    if [ "$self" != "$root/$PROG.sh" ]; then
        cp -- "$self" "$root/.$PROG.new" && chmod 755 -- "$root/.$PROG.new" && mv -f -- "$root/.$PROG.new" "$root/$PROG.sh"
    fi
}

configure() {
    if [ -z "$server_url" ] || [ -z "$workspace" ] || [ -z "$token_file" ]; then
        die "configure needs --server-url, --workspace and --token-file"
    fi
    server_url=${server_url%/}
    check_url "$server_url"
    workspace=$(absolute "$workspace"); token_file=$(absolute "$token_file")
    check_plain_value --workspace "$workspace"; check_plain_value --token-file "$token_file"
    if [ ! -d "$workspace" ] || [ -L "$workspace" ]; then
        die "workspace must be an existing directory (create it with: mkdir -p '$workspace')"
    fi
    [ "$(stat -c '%u' -- "$workspace")" = "$(id -u)" ] || die "workspace must be owned by you: $workspace"
    check_token_file "$token_file"
    if [ -d "$root" ] || [ -L "$root" ]; then
        lock_install
        no_pending_update
    fi
    # Self-update: explicit choice; else keep the earlier one; else on for a
    # new configuration and off for one made before this option existed.
    if [ -z "$self_update" ]; then
        if [ -f "$config" ]; then
            case "$(grep -E '^METRO_RUNNER_SELF_UPDATE=' -- "$config" 2>/dev/null | tail -n1)" in
                *'"1"'|*=1) self_update=on ;;
                *) self_update=off ;;
            esac
        else
            self_update=on
        fi
    fi
    case "$self_update" in on) self_update_value=1 ;; *) self_update_value=0 ;; esac
    mkdir -p -- "$config_dir"
    chmod 700 -- "$config_dir"
    tmp=$(mktemp -- "$config_dir/.runner.env.XXXXXX")
    trap 'rm -f -- "$tmp"; if [ -n "${compat_token:-}" ]; then compat_release; fi' EXIT
    {
        echo "# Metro runner settings (written by $PROG). The token itself is not stored here."
        printf 'METRO_SERVER_URL="%s"\n' "$server_url"
        printf 'METRO_WORKSPACE="%s"\n' "$workspace"
        printf 'METRO_RUNNER_TOKEN_FILE="%s"\n' "$token_file"
        printf 'METRO_RUNNER_SELF_UPDATE="%s"\n' "$self_update_value"
    } >"$tmp"
    chmod 600 -- "$tmp"
    mv -f -- "$tmp" "$config"
    # Older copies can either reject the update setting or reject the URL /
    # send it through a proxy. Replace only copies missing required support;
    # this does not migrate the install marker for managed updates.
    if [ -L "$root/current" ] && { [ ! -f "$root/$PROG.sh" ] ||
        ! grep -q 'METRO_RUNNER_SELF_UPDATE' -- "$root/$PROG.sh" ||
        ! grep -q '^direct_no_proxy()' -- "$root/$PROG.sh"; }; then
        keep_script_copy
        say "Updated the installed copy of this script: $root/$PROG.sh"
    fi
    say "Saved $config"
    say "The runner keeps each attached Space's files in $workspace/spaces/<space-id>/."
    if [ "$self_update" = on ]; then
        say "Self-update is on: when you click Update for this runner in Metro, it downloads that release from GitHub, verifies its signature itself and restarts. Turn it off with: $PROG configure ... --self-update off"
    else
        say "Self-update is off: update this runner with '$PROG update --archive ...'."
    fi
    case "$url_class" in
        loopback) say "Note: the loopback port must stay the same; a server that picks a new port each start needs reconfiguring." ;;
        other) plain_http_warning "$url_host" ;;
    esac
}

run_runner() {
    [ -x "$root/current/run-runner" ] || die "the runner is not installed; run '$PROG install --archive ...' first"
    load_config
    check_url "$cfg_METRO_SERVER_URL"
    check_token_file "$cfg_METRO_RUNNER_TOKEN_FILE"
    [ -d "$cfg_METRO_WORKSPACE" ] || die "workspace does not exist: $cfg_METRO_WORKSPACE"
    problems=$(host_problems "$root/current/metro-runner")
    [ -z "$problems" ] || die "this host cannot run the runner: $problems"
    for k in $cfg_env; do eval "export $k=\"\$cfg_$k\""; done
    unset METRO_RUNNER_TOKEN METRO_ALLOW_INSECURE_HTTP
    # Runners up to 0.4.0 refuse plain http beyond loopback without this
    # opt-in; newer ones accept it, ignore it and print their own warning.
    case "$url_class" in tailscale|other) export METRO_ALLOW_INSECURE_HTTP=1 ;; esac
    case "$url_class" in
        loopback|tailscale)
            direct=$(direct_no_proxy "$url_host")
            export NO_PROXY="$direct" no_proxy="$direct" ;;
    esac
    exec "$root/current/run-runner"
}

# ---------------------------------------------------------------- install
install_runner() {
    [ -n "$archive" ] || die "--archive is required"
    [ -f "$archive" ] || die "archive not found: $archive"
    archive=$(absolute "$archive")
    name=${archive##*/}
    case "$name" in
        Metro-runner-*-linux-x64.tar.gz) top=${name%.tar.gz}; version=${top#Metro-runner-}; version=${version%-linux-x64} ;;
        *) die "expected a Metro-runner-<version>-linux-x64.tar.gz archive, got $name" ;;
    esac
    printf '%s' "$version" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9][A-Za-z0-9.-]{0,40})?$' ||
        die "unexpected version in archive name: $version"
    problems=$(host_problems)
    [ -z "$problems" ] || die "this host cannot run the native runner: $problems"
    # Only the new install root is private; parent directories use the umask.
    # shellcheck disable=SC2174
    mkdir -p -m 700 -- "$root"
    lock_install
    no_pending_update
    [ "$action" != update ] || [ -L "$root/current" ] || die "the runner is not installed in $root; use install"
    if [ ! -f "$root/$MARK" ]; then
        for root_entry in "$root"/* "$root"/.[!.]* "$root"/..?*; do
            [ "$root_entry" != "$root/.lock" ] || continue
            if [ -e "$root_entry" ] || [ -L "$root_entry" ]; then
                die "$root exists but was not created by this installer; refusing to change it"
            fi
        done
    fi
    stage=
    old=
    installed=0
    trap 'if [ "$root" -ef /proc/self/fd/9 ]; then
        if [ "$installed" = 0 ] && [ -n "$old" ] && [ ! -e "$root/versions/$version" ] && [ -e "$old/$version" ]; then
            mv -- "$old/$version" "$root/versions/$version"
        fi
        [ -z "$stage" ] || rm -rf -- "$stage"
        compat_release
    fi' EXIT
    trap 'exit 130' INT TERM
    stage=$(mktemp -d -- "$root/.stage.XXXXXX")
    # Verify the same private copy that tar will read.
    source_archive=$archive
    cp -- "$source_archive" "$stage/$name"
    if [ -z "$sha" ] && [ -z "$checksum_file" ]; then
        if [ -f "$source_archive.sha256" ]; then checksum_file=$source_archive.sha256
        elif [ -f "${source_archive%/*}/SHA256SUMS" ]; then checksum_file=${source_archive%/*}/SHA256SUMS
        else die "no checksum for $name: pass --sha256 HEX or --checksum-file FILE"
        fi
    fi
    archive=$stage/$name
    verify_archive_checksum "$archive"
    extract_verified "$archive" "$stage" "$top"
    app=$stage/$top
    if [ "$(json_field "$app/RELEASE.json" kind)" != runner ] ||
        [ "$(json_field "$app/RELEASE.json" target)" != linux-x64 ] ||
        [ "$(json_field "$app/RELEASE.json" version)" != "$version" ]; then
        die "RELEASE.json does not describe a Metro runner $version linux-x64 package"
    fi
    if [ ! -x "$app/metro-runner" ] || [ ! -x "$app/run-runner" ]; then
        die "package is missing metro-runner or run-runner"
    fi
    problems=$(host_problems "$app/metro-runner")
    [ -z "$problems" ] || die "this host cannot run this runner: $problems"
    if [ ! -e "$root/$MARK" ]; then
        (set -C; echo "Metro runner per-user install (managed by $PROG)." >"$root/$MARK")
    fi
    [ ! -L "$root/versions" ] || die "versions directory is a symlink"
    mkdir -p -- "$root/versions"
    if [ -e "$root/versions/$version" ]; then
        old=$(mktemp -d -- "$root/.old.XXXXXX"); mv -- "$root/versions/$version" "$old/"
    fi
    mv -- "$app" "$root/versions/$version"
    ln -sfn -- "versions/$version" "$root/.current.new"
    mv -fT -- "$root/.current.new" "$root/current"
    installed=1
    # Don't prune previously working copies as part of a manual switch.
    [ -z "$old" ] || rm -rf -- "$old"
    # Keep a copy of this script beside the install for `run` and the unit.
    keep_script_copy
    if [ "$migrate" = 1 ]; then
        migration=$(mktemp -- "$root/.migrate.XXXXXX")
        printf '%s\n' metro-update-lock-v2 >"$migration"
        mv -fT -- "$migration" "$root/$MARK"
    fi
    say "Metro runner $version is installed in $root/versions/$version"
    if [ -f "$config" ]; then
        say "Existing settings kept: $config"
        if [ -f "$unit_file" ]; then
            say "Restart it to use the new version: systemctl --user restart $UNIT"
        fi
    else
        say "Next: $root/$PROG.sh configure --server-url https://metro.example --workspace DIR --token-file FILE"
    fi
}

install_service() {
    lock_install
    no_pending_update
    [ -x "$root/$PROG.sh" ] || die "install the runner first"
    load_config
    check_plain_value "install directory" "$root"
    check_plain_value "config directory" "$config_home"
    mkdir -p -- "${unit_file%/*}"
    if [ -f "$unit_file" ] && ! grep -qxF "$UNIT_TAG" "$unit_file"; then
        die "$unit_file exists and was not written by this script; refusing to replace it"
    fi
    tmp=$(mktemp -- "${unit_file%/*}/.$UNIT.XXXXXX")
    {
        echo "$UNIT_TAG"
        echo "[Unit]"
        echo "Description=Metro runner (outbound only)"
        echo "After=network-online.target"
        echo "Wants=network-online.target"
        echo
        echo "[Service]"
        printf 'Environment="XDG_CONFIG_HOME=%s" "METRO_RUNNER_INSTALL_DIR=%s"\n' "$config_home" "$root"
        printf 'ExecStart="%s" run\n' "$root/$PROG.sh"
        echo "Restart=on-failure"
        echo "RestartSec=10"
        echo "NoNewPrivileges=yes"
        echo
        echo "[Install]"
        echo "WantedBy=default.target"
    } >"$tmp"
    chmod 644 -- "$tmp"
    mv -f -- "$tmp" "$unit_file"
    say "Wrote $unit_file (not started)."
    say "Start it now and at login with: systemctl --user daemon-reload && systemctl --user enable --now $UNIT"
}

doctor() {
    ok=1
    problems=$(host_problems "$root/current/metro-runner")
    if [ -n "$problems" ]; then ok=0; printf 'problem: %s\n' "$problems"; fi
    if [ -L "$root/current" ]; then say "installed: $(readlink -- "$root/current")"; else ok=0; say "problem: not installed in $root"; fi
    if [ -f "$config" ]; then
        if msg=$( (load_config && check_url "$cfg_METRO_SERVER_URL" &&
            check_token_file "$cfg_METRO_RUNNER_TOKEN_FILE" &&
            { [ -d "$cfg_METRO_WORKSPACE" ] || die "workspace does not exist: $cfg_METRO_WORKSPACE"; }) 2>&1); then
            say "configuration: OK ($config)"
        else
            ok=0; say "problem: ${msg#"$PROG: "}"
        fi
    else
        ok=0; say "problem: not configured ($config)"
    fi
    [ "$ok" = 1 ] && say "Everything looks ready." || exit 1
}

uninstall_runner() {
    locked=
    if [ -d "$root" ] || [ -L "$root" ]; then
        lock_install
        no_pending_update
        [ -f "$root/$MARK" ] || die "$root was not created by this installer; refusing to remove it"
        locked=1
    fi
    if [ -f "$unit_file" ] && grep -qxF "$UNIT_TAG" "$unit_file"; then
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --user disable --now "$UNIT" >/dev/null 2>&1 || true
        fi
        rm -f -- "$unit_file"
        if command -v systemctl >/dev/null 2>&1; then
            systemctl --user daemon-reload >/dev/null 2>&1 || true
        fi
        say "Removed $unit_file"
    fi
    if [ "$purge" = 1 ]; then
        rm -f -- "$config"; rmdir -- "$config_dir" 2>/dev/null || true
        say "Removed $config"
    elif [ -f "$config" ]; then
        say "Settings kept in $config (use --purge-config to remove them)."
    fi
    if [ -n "$locked" ]; then
        [ "$root" -ef /proc/self/fd/9 ] || die "the install directory changed"
        retired=$(mktemp -d -- "${root%/*}/.metro-uninstall.XXXXXX")
        mv -T -- "$root" "$retired/install"
        rm -rf -- "$retired"
        say "Removed $root"
    fi
    say "Your workspace and token file were not changed. Revoke the runner's token in Metro if you no longer use it."
}

case "$action" in
    install|update) install_runner ;;
    configure) configure ;;
    run) run_runner ;;
    install-service) install_service ;;
    doctor) doctor ;;
    status)
        if [ -L "$root/current" ]; then say "installed: $(readlink -- "$root/current") in $root"; else say "not installed in $root"; fi
        if [ -f "$config" ]; then say "settings: $config"; else say "not configured"; fi
        if [ -f "$config" ] && grep -qE '^METRO_RUNNER_SELF_UPDATE="?1"?$' -- "$config"; then say "self-update: on"; else say "self-update: off"; fi
        if [ -f "$root/.update-pending.json" ]; then say "update in progress: $(cat -- "$root/.update-pending.json")"; fi
        if [ -f "$root/.update-failed.json" ]; then say "last self-update failed: $(cat -- "$root/.update-failed.json")"; fi ;;
    uninstall) uninstall_runner ;;
    -h|--help|help) usage ;;
    '') usage >&2; exit 2 ;;
    *) usage >&2; die "unknown command: $action" ;;
esac
