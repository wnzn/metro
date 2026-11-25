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

--server-url must be https://, or http:// to this machine's loopback
(127.0.0.1, ::1 or localhost) on a stable port.
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
    if [ -e "$root/.lock" ] || [ -L "$root/.lock" ]; then
        [ ! -L "$root/.lock" ] && [ ! -L "$compat_owner" ] && [ -f "$compat_owner" ] &&
            [ "$(ls -A -- "$root/.lock")" = owner.metro-lock-v2 ] &&
            [ "$(stat -c '%s' -- "$compat_owner")" -le 128 ] &&
            [ "$(wc -l <"$compat_owner")" -eq 1 ] &&
            grep -Eq '^metro-lock-v2:[A-Za-z0-9._-]+$' "$compat_owner" &&
            compat_retire || die "another install holds the legacy lock at $root/.lock"
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
archive='' sha='' checksum_file='' server_url='' workspace='' token_file='' purge=0 self_update= migrate=0
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
# Plain http only to an exact loopback literal (localhost, 127.0.0.1 or
# [::1]) with an explicit numeric port 1-65535, then an optional path.
loopback_http() { # $1 = URL starting with http://
    rest=${1#http://}; auth=${rest%%/*}
    case "$auth" in
        localhost:*) host=localhost; port=${auth#localhost:} ;;
        127.0.0.1:*) host=127.0.0.1; port=${auth#127.0.0.1:} ;;
        \[::1\]:*) host='[::1]'; port=${auth#\[::1\]:} ;;
        *) return 1 ;;
    esac
    [ "$auth" = "$host:$port" ] || return 1
    case "$port" in ''|*[!0-9]*) return 1 ;; esac
    [ ${#port} -le 5 ] && [ "$port" -ge 1 ] && [ "$port" -le 65535 ]
}

check_url() {
    u=$1
    case "$u" in *[[:space:]]*|*\?*|*\#*|*@*|*\"*|*\\*|*\$*|*\`*) die "--server-url must be a plain URL without spaces, credentials, query or fragment" ;; esac
    case "$u" in
        https://[!/:]*) ;;
        http://?*) loopback_http "$u" ||
            die "plain http to a non-loopback host would send the runner token unencrypted; use an https:// URL (or http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT on this machine)" ;;
        *) die "--server-url must start with https:// (or http:// for loopback)" ;;
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
    # An installed copy of an older script would refuse the new setting on
    # its next `run`: replace it with this one (never a newer copy).
    if [ -L "$root/current" ] && { [ ! -f "$root/$PROG.sh" ] ||
        ! grep -q 'METRO_RUNNER_SELF_UPDATE' -- "$root/$PROG.sh"; }; then
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
    case "$server_url" in http://*) say "Note: the loopback port must stay the same; a server that picks a new port each start needs reconfiguring." ;; esac
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
    mkdir -p -m 700 -- "$root"
    lock_install
    no_pending_update
    [ "$action" != update ] || [ -L "$root/current" ] || die "the runner is not installed in $root; use install"
    if [ -n "$(ls -A -- "$root" | grep -vxF .lock || :)" ] && [ ! -f "$root/$MARK" ]; then
        die "$root exists but was not created by this installer; refusing to change it"
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
