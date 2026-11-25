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
# $1 = a FIXED shell command (never built from settings); any further
# arguments reach it as $1, $2, ... inside the container, as data.
probe() {
    _probe_cmd=$1; shift
    # shellcheck disable=SC2046
    "$engine" run --rm --pull=never $(network_flag) --read-only --cap-drop ALL \
        --security-opt no-new-privileges:true \
        --mount "type=bind,source=$token_file,target=$SECRET,readonly" \
        --entrypoint /bin/bash "$image" -c "$_probe_cmd" metro-probe "$@" >/dev/null 2>&1
}

# Can the container open a TCP connection to host $1, port $2?
# shellcheck disable=SC2016 # expanded by the container shell, from its arguments
PROBE_TCP='exec 3<>"/dev/tcp/$1/$2"'

# The runner container's entry: adds the validated Metro host
# (METRO_DIRECT_NO_PROXY, loopback or Tailscale only) to the container's own
# NO_PROXY, keeping exclusions the engine injected, then reads the token from
# the read-only mount and starts the runner. Fixed text: settings reach it
# only as environment values. Same text as $RunnerEntry in
# metro-runner-container.ps1.
# shellcheck disable=SC2016 # expanded by the container shell
RUNNER_ENTRY='if [ -n "${METRO_DIRECT_NO_PROXY:-}" ]; then NO_PROXY="${NO_PROXY-${no_proxy-}}"; NO_PROXY="${NO_PROXY:+$NO_PROXY,}$METRO_DIRECT_NO_PROXY"; no_proxy="$NO_PROXY"; export NO_PROXY no_proxy; fi; unset METRO_DIRECT_NO_PROXY; IFS= read -r METRO_RUNNER_TOKEN < /run/secrets/metro_runner_token || [ -n "$METRO_RUNNER_TOKEN" ]; export METRO_RUNNER_TOKEN; exec /usr/local/bin/metro-runner'

# Runner images up to 0.4.0 refuse plain http beyond loopback without this
# opt-in; newer ones accept it, ignore it and print their own warning.
insecure_flag() {
    case "$url_class" in
        tailscale|other) printf '%s' '--env METRO_ALLOW_INSECURE_HTTP=1' ;;
    esac
}

# A loopback or Tailscale server is always reached directly: the entry script
# adds exactly that host to the container's NO_PROXY, after any exclusions
# the engine injected (Docker's "proxies" client setting), which are kept.
# Other destinations keep the engine's settings as they are.
direct_flag() {
    case "$url_class" in
        loopback|tailscale)
            direct=$(unset NO_PROXY no_proxy; direct_no_proxy "$url_host")
            printf -- '--env METRO_DIRECT_NO_PROXY=%s' "$direct" ;;
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
    # shellcheck disable=SC2016 # expanded by the container shell, from its argument
    probe 'test -r "$1" && test -s "$1"' "$SECRET" ||
        die "the container cannot read $token_file. Share its folder with $engine (Docker Desktop: Settings > Resources > File sharing); on Linux the file must be readable by the image's runner user (uid 1000), or use the native Linux runner."
    if [ "$network" = host ]; then
        # url_host and url_port come from http_class (loopback: an exact
        # literal and a numeric port); they reach the probe as arguments.
        case "$url_host" in \[::1\]) h=::1 ;; *) h=127.0.0.1 ;; esac
        if ! probe "$PROBE_TCP" "$h" "$url_port"; then
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
    set -f # the unquoted flags hold IPv6 brackets: never glob them
    # shellcheck disable=SC2016 # expanded by the container shell
    # The token is read from the read-only mount at start, never put in the
    # container's configuration, the command line or the image.
    # shellcheck disable=SC2046
    "$engine" run -d --name "$name" --restart unless-stopped --pull=never $(network_flag) \
        --read-only --tmpfs /tmp:rw,noexec,nosuid,nodev,size=64m \
        --cap-drop ALL --security-opt no-new-privileges:true --pids-limit 256 \
        --mount "type=volume,source=$volume,target=/workspace" \
        --mount "type=bind,source=$token_file,target=$SECRET,readonly" \
        --env "METRO_SERVER_URL=$server_url" $(insecure_flag) $(direct_flag) \
        --label dev.metro.managed-by=$PROG \
        --entrypoint /bin/sh "$image" \
        -c "$RUNNER_ENTRY" \
        >/dev/null || { set +f; die "could not start the runner container"; }
    set +f
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
