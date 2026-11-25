#!/bin/sh
# Metro runner in a container (for macOS, or Linux hosts that prefer it):
# runs the official prebuilt Linux runner image with Docker or Podman. The
# runner only makes outbound connections; no port is published. This script
# never creates, rotates or prints a runner token: create one in Metro, save
# it to a mode-600 file and give this script its path.
set -eu
umask 077

PROG=metro-runner-container
SECRET=/run/secrets/metro_runner_token
DEFAULT_REPO=ghcr.io/wnzn/metro-runner

die() { printf '%s: %s\n' "$PROG" "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
    cat <<'EOF'
Usage:
  metro-runner-container.sh setup --server-url URL --token-file FILE
                                  (--version X.Y.Z | --image REF)
                                  [--volume NAME] [--name NAME] [--engine docker|podman]
  metro-runner-container.sh start | stop | restart | status | logs
  metro-runner-container.sh update (--version X.Y.Z | --image REF)
  metro-runner-container.sh remove [--purge-config]

setup saves settings (no token) and checks that the image is available and
the container can read the token file. start runs the container in the
background; it restarts with the engine unless you stop it.

--server-url  https://..., or http:// to any host. For a Metro server on
              this computer use http://127.0.0.1:PORT (stable port); loopback
              needs host networking: native on Linux; Docker Desktop 4.34+
              only with Settings > Resources > Network > Enable host
              networking. Other hosts (a LAN address, Tailscale) need no
              host networking. Plain http to a host other than this computer
              or a Tailscale address works, with a warning: the runner token
              crosses the network unencrypted.
--token-file  regular file owned by you, mode 600, holding the runner token.
--volume      named volume mounted at /workspace (default: metro-runner-workspace).
              The runner keeps each Space's files in spaces/<space-id>/.
              remove never deletes it.
--image       full image reference (pin a digest with REPO@sha256:...);
              --version X.Y.Z uses ghcr.io/wnzn/metro-runner:vX.Y.Z.
EOF
}

absolute() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$(pwd -P)" "$1" ;; esac; }
xdg_dir() { case "${1:-}" in /*) printf '%s' "$1" ;; *) printf '%s' "$2" ;; esac; }

case "${HOME:-}" in /*) ;; *) die "HOME must be set to an absolute path" ;; esac
config_dir=$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")/metro-runner
config=$config_dir/container.env

action=${1:-}
[ $# -gt 0 ] && shift
server_url='' token_file='' image='' version='' volume='' name='' engine='' purge=0
while [ $# -gt 0 ]; do
    case "$1" in
        --server-url) [ $# -ge 2 ] || die "--server-url needs a URL"; server_url=$2; shift 2 ;;
        --token-file) [ $# -ge 2 ] || die "--token-file needs a file"; token_file=$2; shift 2 ;;
        --image) [ $# -ge 2 ] || die "--image needs a reference"; image=$2; shift 2 ;;
        --version) [ $# -ge 2 ] || die "--version needs X.Y.Z"; version=${2#v}; shift 2 ;;
        --volume) [ $# -ge 2 ] || die "--volume needs a name"; volume=$2; shift 2 ;;
        --name) [ $# -ge 2 ] || die "--name needs a name"; name=$2; shift 2 ;;
        --engine) [ $# -ge 2 ] || die "--engine needs docker or podman"; engine=$2; shift 2 ;;
        --purge-config) purge=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

# ---------------------------------------------------------------- checks
# url-rule:begin (test_installers.py runs this block against
# crates/url-policy/testdata/host_vectors.json)
# Plain http works to any host. The runner token is sent
# as given, so the host decides how it's said:
# - loopback: only the exact literals localhost, 127.0.0.1 or [::1] with an
#   explicit port 1-65535 (anything else that names this machine, such as
#   127.1, LOCALHOST, [::ffff:7f00:1] or a trailing dot, is refused as
#   ambiguous: a trailing dot sends even an address through the resolver);
# - Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48, names ending in .ts.net):
#   encrypted by Tailscale itself, used silently;
# - any other host: allowed, with one warning that the token crosses the
#   network unencrypted.
# http_class URL sets url_class (loopback, tailscale or other) and url_host;
# it fails on a malformed or ambiguous authority.
http_class() {
    rest=${1#http://}; auth=${rest%%/*}; port='' has_port=0 url_class='' url_host=
    case "$auth" in
        \[*\]:*) url_host=${auth%%\]:*}']'; port=${auth##*\]:}; has_port=1 ;;
        \[*\]) url_host=$auth ;;
        *:*:*) return 1 ;;
        *:*) url_host=${auth%:*}; port=${auth##*:}; has_port=1 ;;
        *) url_host=$auth ;;
    esac
    if [ "$has_port" = 1 ]; then
        case "$port" in ''|*[!0-9]*) return 1 ;; esac
        [ ${#port} -le 5 ] && [ "$port" -ge 1 ] && [ "$port" -le 65535 ] || return 1
    fi
    case "$url_host" in
        \[*\]) inner=${url_host#\[}; inner=${inner%\]}
               case "$inner" in ''|*[!0-9A-Fa-f:]*|::[fF][fF][fF][fF]:*) return 1 ;; esac ;;
        # Labels of letters, digits and inner hyphens; no trailing dot.
        ''|.*|-*|*.|*-|*..*|*-.*|*.-*|*[!A-Za-z0-9.-]*) return 1 ;;
    esac
    if [ "$has_port" = 1 ]; then
        case "$url_host" in localhost|127.0.0.1|\[::1\]) url_class=loopback; return 0 ;; esac
    fi
    lower=$(printf '%s' "$url_host" | tr '[:upper:]' '[:lower:]')
    case "$lower" in
        localhost*|127.*|0.0.0.0|0) return 1 ;;
        \[*\]) bits=$(printf '%s' "$lower" | tr -d '[]0:')
               [ -n "$bits" ] && [ "$bits" != 1 ] || return 1 ;;
    esac
    case "$lower" in
        *.ts.net) url_class=tailscale; return 0 ;;
        \[fd7a:115c:a1e0:*\]) url_class=tailscale; return 0 ;;
    esac
    case "$lower" in
        *[!0-9.]*) ;;
        *.*.*.*)
            o1=${lower%%.*}; r=${lower#*.}; o2=${r%%.*}; r=${r#*.}; o3=${r%%.*}; o4=${r#*.}
            for o in "$o1" "$o2" "$o3" "$o4"; do
                case "$o" in ''|*[!0-9]*) return 1 ;; esac
                [ ${#o} -le 3 ] && [ "$o" -le 255 ] || return 1
            done
            if [ "$o1" = 100 ] && [ "$o2" -ge 64 ] && [ "$o2" -le 127 ]; then url_class=tailscale; return 0; fi ;;
    esac
    url_class=other
}

# url-rule:end

plain_http_warning() { # $1 = host; never mentions the token's value
    printf '%s: warning: plain HTTP to %s isn'"'"'t encrypted: anyone on the network between this machine and %s could read the runner token. Use it only on networks you trust, such as Tailscale or a VPN, or use https://.\n' "$PROG" "$1" "$1" >&2
}

check_url() { # sets url_class (https, loopback, tailscale or other)
    case "$1" in *[[:space:]]*|*\?*|*\#*|*@*|*\"*|*\\*|*\$*|*\`*|*,*) die "--server-url must be a plain URL without spaces, credentials, query or fragment" ;; esac
    case "$1" in
        https://[!/:]*) url_class=https ;;
        http://?*) http_class "$1" ||
            die "invalid --server-url $1: for this computer use exactly http://127.0.0.1:PORT, http://localhost:PORT or http://[::1]:PORT; otherwise host[:port] with a port of 1-65535" ;;
        *) die "--server-url must start with https:// or http://" ;;
    esac
}

file_owner_mode() {
    stat -c '%u:%a' -- "$1" 2>/dev/null || stat -f '%u:%Lp' -- "$1"
}

check_token_file() { # never reads the token
    case "$1" in /*) ;; *) die "token file path must be absolute" ;; esac
    case "$1" in *,*|*:*|*"
"*) die "token file path must not contain commas, colons or newlines: $1" ;; esac
    if [ -L "$1" ] || [ ! -f "$1" ]; then
        die "token file must be a regular file (not a link): $1"
    fi
    [ -s "$1" ] || die "token file is empty: $1"
    [ "$(file_owner_mode "$1")" = "$(id -u):600" ] || die "token file must be owned by you with mode 600 (run: chmod 600 '$1')"
}

check_name() { # container / volume names
    printf '%s' "$2" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9_.-]{0,62}$' || die "invalid $1: $2"
}

check_image() {
    printf '%s' "$1" | grep -Eq '^[a-z0-9][a-z0-9._/:-]*(@sha256:[0-9a-f]{64})?$' || die "invalid image reference: $1"
}

pick_engine() {
    if [ -n "$engine" ]; then
        case "$engine" in docker|podman) ;; *) die "--engine must be docker or podman" ;; esac
    elif command -v docker >/dev/null 2>&1; then engine=docker
    elif command -v podman >/dev/null 2>&1; then engine=podman
    else die "Docker or Podman is required (install Docker Desktop, Docker Engine or Podman first)"
    fi
    command -v "$engine" >/dev/null 2>&1 || die "$engine is not installed"
    "$engine" info >/dev/null 2>&1 || die "$engine is installed but not running (start Docker Desktop / the $engine service)"
}

write_config() {
    mkdir -p -- "$config_dir"; chmod 700 -- "$config_dir"
    tmp=$(mktemp -- "$config_dir/.container.env.XXXXXX")
    {
        echo "# Metro runner container settings (written by $PROG). The token itself is not stored here."
        printf 'ENGINE=%s\nIMAGE=%s\nNAME=%s\nVOLUME=%s\nNETWORK=%s\n' "$engine" "$image" "$name" "$volume" "$network"
        printf 'SERVER_URL=%s\nTOKEN_FILE=%s\n' "$server_url" "$token_file"
    } >"$tmp"
    chmod 600 -- "$tmp"; mv -f -- "$tmp" "$config"
}

load_config() {
    [ -f "$config" ] || die "not set up: run '$PROG setup --server-url URL --token-file FILE --version X.Y.Z'"
    [ "$(file_owner_mode "$config")" = "$(id -u):600" ] || die "$config must be owned by you with mode 600"
    while IFS= read -r line || [ -n "$line" ]; do
        case "$line" in ''|'#'*) continue ;; esac
        k=${line%%=*}; v=${line#*=}
        case "$k" in
            ENGINE) engine=${engine:-$v} ;; IMAGE) image=${image:-$v} ;; NAME) name=$v ;; VOLUME) volume=$v ;;
            NETWORK) network=$v ;; SERVER_URL) server_url=$v ;; TOKEN_FILE) token_file=$v ;;
            *) die "$config: unsupported setting $k" ;;
        esac
    done <"$config"
    check_url "$server_url"; check_token_file "$token_file"; check_image "$image"
    check_name "container name" "$name"; check_name volume "$volume"
    case "$network" in host|default) ;; *) die "$config: NETWORK must be host or default" ;; esac
}

exists() { "$engine" container inspect "$name" >/dev/null 2>&1; }

ensure_image() {
    if ! "$engine" image inspect "$image" >/dev/null 2>&1; then
        say "Pulling $image ..."
        "$engine" pull "$image" >/dev/null ||
            die "cannot pull $image. Check the version and your network; Metro runner images are only available once that version is published."
    fi
}

# Runs a command in the runner image with the same mounts as the real container.
probe() { # $@ = shell command
    # shellcheck disable=SC2046
    "$engine" run --rm --pull=never $(network_flag) --read-only --cap-drop ALL \
        --security-opt no-new-privileges:true \
        --mount "type=bind,source=$token_file,target=$SECRET,readonly" \
        --entrypoint /bin/bash "$image" -c "$1" >/dev/null 2>&1
}

# Runner images up to 0.4.0 refuse plain http beyond loopback without this
# opt-in; newer ones accept it, ignore it and print their own warning.
insecure_flag() {
    case "$url_class" in
        tailscale|other) printf '%s' '--env METRO_ALLOW_INSECURE_HTTP=1' ;;
    esac
}

network_flag() {
    if [ "$network" = host ]; then
        printf '%s' '--network=host' || true
    fi
}

setup() {
    if [ -z "$server_url" ] || [ -z "$token_file" ]; then
        die "setup needs --server-url and --token-file"
    fi
    [ -n "$image" ] || [ -n "$version" ] || die "setup needs --version X.Y.Z or --image REF"
    if [ -z "$image" ]; then
        printf '%s' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$' || die "invalid --version: $version"
        image=$DEFAULT_REPO:v$version
    fi
    server_url=${server_url%/}
    check_url "$server_url"
    token_file=$(absolute "$token_file"); check_token_file "$token_file"
    check_image "$image"
    name=${name:-metro-runner}; volume=${volume:-metro-runner-workspace}
    check_name "container name" "$name"; check_name volume "$volume"
    case "$url_class" in loopback) network=host ;; *) network=default ;; esac
    [ "$url_class" != other ] || plain_http_warning "$url_host"
    pick_engine
    ensure_image
    "$engine" volume inspect "$volume" >/dev/null 2>&1 || "$engine" volume create "$volume" >/dev/null ||
        die "cannot create volume $volume"
    probe "test -r $SECRET && test -s $SECRET" ||
        die "the container cannot read $token_file. Share its folder with $engine (Docker Desktop: Settings > Resources > File sharing); on Linux the file must be readable by the image's runner user (uid 1000), or use the native Linux runner."
    if [ "$network" = host ]; then
        hostport=${server_url#http://}; hostport=${hostport%%/*}
        h=${hostport%:*}; p=${hostport##*:}  # both validated by http_class (loopback)
        case "$h" in localhost) h=127.0.0.1 ;; \[::1\]) h=::1 ;; esac
        if ! probe "exec 3<>/dev/tcp/$h/$p"; then
            say "warning: the container cannot reach $server_url right now. Make sure Metro is running on that port."
            say "         On Docker Desktop, enable Settings > Resources > Network > Enable host networking (4.34+)."
        fi
    fi
    write_config
    say "Saved $config (the token is not stored there)."
    say "Workspace volume: $volume (kept by stop, update and remove)."
    say "Start it with: $0 start"
}

start() {
    load_config; pick_engine
    if exists; then
        if [ "$("$engine" container inspect -f '{{.State.Running}}' "$name")" = true ]; then
            say "$name is already running."; return 0
        fi
        "$engine" start "$name" >/dev/null; say "Started $name."; return 0
    fi
    ensure_image
    # shellcheck disable=SC2016 # expanded by the container shell
    # The token is read from the read-only mount at start, never put in the
    # container's configuration, the command line or the image.
    # shellcheck disable=SC2046
    "$engine" run -d --name "$name" --restart unless-stopped --pull=never $(network_flag) \
        --read-only --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m \
        --cap-drop ALL --security-opt no-new-privileges:true --pids-limit 256 \
        --mount "type=volume,source=$volume,target=/workspace" \
        --mount "type=bind,source=$token_file,target=$SECRET,readonly" \
        --env "METRO_SERVER_URL=$server_url" $(insecure_flag) \
        --label dev.metro.managed-by=$PROG \
        --entrypoint /bin/sh "$image" \
        -c 'IFS= read -r METRO_RUNNER_TOKEN < '"$SECRET"' || [ -n "$METRO_RUNNER_TOKEN" ]; export METRO_RUNNER_TOKEN; exec /usr/local/bin/metro-runner' \
        >/dev/null || die "could not start the runner container"
    say "Started $name ($image). See its log with: $0 logs"
}

managed() {
    [ "$("$engine" container inspect -f '{{index .Config.Labels "dev.metro.managed-by"}}' "$name" 2>/dev/null)" = "$PROG" ] ||
        die "container $name was not created by this script; refusing to change it"
}

stop() {
    load_config; pick_engine
    exists || { say "$name is not running."; return 0; }
    managed
    "$engine" stop "$name" >/dev/null; say "Stopped $name."
}

remove_container() {
    exists || return 0
    managed
    "$engine" rm -f "$name" >/dev/null
}

case "$action" in
    setup) setup ;;
    start) start ;;
    stop) stop ;;
    restart) load_config; pick_engine; remove_container; start ;;
    status)
        load_config; pick_engine
        if exists; then "$engine" container inspect -f "$name: {{.State.Status}} ({{.Config.Image}})" "$name"
        else say "$name: not created"; fi
        say "settings: $config" ;;
    logs) load_config; pick_engine; exists || die "$name does not exist"; "$engine" logs --tail 200 "$name" ;;
    update)
        [ -n "$image" ] || [ -n "$version" ] || die "update needs --version X.Y.Z or --image REF"
        [ -n "$image" ] || image=$DEFAULT_REPO:v$version
        new_image=$image; image=
        load_config; image=$new_image; check_image "$image"; pick_engine
        ensure_image
        remove_container
        write_config
        start ;;
    remove)
        if [ -f "$config" ]; then
            load_config; pick_engine; remove_container; say "Removed container $name."
            say "Volume $volume was kept (delete it yourself with: $engine volume rm $volume)."
        fi
        if [ "$purge" = 1 ]; then rm -f -- "$config"; say "Removed $config"; fi
        say "Your token file was not changed. Revoke the runner's token in Metro if you no longer use it." ;;
    -h|--help|help) usage ;;
    '') usage >&2; exit 2 ;;
    *) usage >&2; die "unknown command: $action" ;;
esac
