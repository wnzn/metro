#!/bin/sh
# Metro desktop for Linux x64: per-user install, update and uninstall from a
# prebuilt Metro-<version>-linux-x64.tar.gz. No sudo is needed unless you opt
# in to --install-deps. Your Metro data (~/.config/metro-desktop) is never
# touched by this script.
set -eu
umask 022

PROG=install-metro-linux
MARK=.metro-desktop-install
GLIBC_MIN=2.39

die() { printf '%s: %s\n' "$PROG" "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
    cat <<'EOF'
Usage:
  install-metro-linux.sh install   --archive FILE [options]
  install-metro-linux.sh update    --archive FILE [options]
  install-metro-linux.sh uninstall
  install-metro-linux.sh check-deps [--install-deps]
  install-metro-linux.sh status  (read-only, including update journal diagnosis)

Options for install/update:
  --archive FILE        Metro-<version>-linux-x64.tar.gz (a local file)
  --sha256 HEX          expected SHA-256 of the archive
  --checksum-file FILE  a .sha256 sidecar or SHA256SUMS that lists the archive
                        (default: FILE.sha256 or SHA256SUMS next to the archive)
  --no-local-runner     do not enable Metro's local runner support
  --local-runner        enable it (the default for a new install; updates
                        keep your previous choice)
  --install-deps        if GTK 4 / WebKitGTK 6.0 / GStreamer are missing, run
                        your package manager (with sudo) for a fixed package
                        list, then check again
  --no-menu             do not add Metro to the applications menu
  --migrate-update-lock enable managed updates using the current installer
                        with update; close Metro first and retire old copies

Environment: METRO_INSTALL_DIR overrides the install directory
(default: ${XDG_DATA_HOME:-~/.local/share}/metro-desktop-app).
EOF
}

absolute() {
    case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$(pwd -P)" "$1" ;; esac
}

xdg_dir() { # $1 = value, $2 = default (XDG ignores relative values)
    case "${1:-}" in /*) printf '%s' "$1" ;; *) printf '%s' "$2" ;; esac
}

case "${HOME:-}" in /*) ;; *) die "HOME must be set to an absolute path" ;; esac
data_home=$(xdg_dir "${XDG_DATA_HOME:-}" "$HOME/.local/share")
config_home=$(xdg_dir "${XDG_CONFIG_HOME:-}" "$HOME/.config")
root=$(xdg_dir "${METRO_INSTALL_DIR:-}" "$data_home/metro-desktop-app")
user_data=$config_home/metro-desktop
workspace_data=$(xdg_dir "${METRO_DESKTOP_HOME:-}" "$user_data")

# ---------------------------------------------------------------- arguments
action=${1:-}
[ $# -gt 0 ] && shift
archive='' sha='' checksum_file='' local_runner='' install_deps=0 menu=1 migrate=0
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) [ $# -ge 2 ] || die "--archive needs a file"; archive=$2; shift 2 ;;
        --sha256) [ $# -ge 2 ] || die "--sha256 needs a value"; sha=$2; shift 2 ;;
        --migrate-update-lock) migrate=1; shift ;;
        --checksum-file) [ $# -ge 2 ] || die "--checksum-file needs a file"; checksum_file=$2; shift 2 ;;
        --no-local-runner) local_runner=no; shift ;;
        --local-runner) local_runner=yes; shift ;;
        --install-deps) install_deps=1; shift ;;
        --no-menu) menu=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done
[ "$migrate" = 0 ] || [ "$action" = update ] || die "--migrate-update-lock requires update"

# ---------------------------------------------------------------- checksums
sha_of() { sha256sum -- "$1" | cut -d' ' -f1; }

lookup_sum() { # $1 = checksum file, $2 = archive basename
    awk -v n="$2" '($2 == n || $2 == "*" n) && $1 ~ /^[0-9a-fA-F]+$/ && length($1) == 64 { print tolower($1); f++ }
                   END { exit f == 1 ? 0 : 1 }' "$1"
}

verify_archive_checksum() { # $1 = archive
    name=${1##*/}
    if [ -n "$sha" ]; then
        expected=$(printf '%s' "$sha" | tr 'A-F' 'a-f')
        source="--sha256"
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

# Lists RELEASE.json "files" as "<sha256>  <path>" lines (package.py's fixed
# JSON layout, the same in 0.2.0 archives built before the installers existed).
manifest_sums() { # $1 = RELEASE.json
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

json_field() { # $1 = RELEASE.json, $2 = top-level string key
    sed -n "s/^  \"$2\": \"\\([^\"\\\\]*\\)\",\\{0,1\\}\$/\\1/p" "$1"
}

# Extract a verified tar.gz into $2 and check every file against RELEASE.json.
extract_verified() { # $1 = archive, $2 = destination, $3 = expected top folder
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

# ---------------------------------------------------------------- runtime libraries
os_release=${METRO_OS_RELEASE_FILE:-/etc/os-release} # test hook

distro_family() {
    [ -r "$os_release" ] || { echo unknown; return; }
    ids=$(sed -n 's/^\(ID\|ID_LIKE\)=//p' "$os_release" | tr -d '"' | tr '\n' ' ')
    for id in $ids; do
        case "$id" in
            arch|cachyos|endeavouros|manjaro|garuda) echo arch; return ;;
            debian|ubuntu|linuxmint|pop) echo debian; return ;;
            fedora) echo fedora; return ;;
        esac
    done
    echo unknown
}

deps_command() { # the fixed, bounded package list per family
    case "$(distro_family)" in
        arch) echo "pacman -S --needed webkitgtk-6.0 gtk4 gst-plugins-base-libs gst-plugins-base" ;;
        debian) echo "apt-get install --no-install-recommends libgtk-4-1 libwebkitgtk-6.0-4 gstreamer1.0-plugins-base" ;;
        fedora) echo "dnf install webkitgtk6.0 gtk4 gstreamer1-plugins-base" ;;
        *) echo "" ;;
    esac
}

glibc_ok() {
    v=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc //p')
    [ -n "$v" ] || return 1
    [ "$(printf '%s\n%s\n' "$GLIBC_MIN" "$v" | sort -V | head -n1)" = "$GLIBC_MIN" ]
}

missing_libs() { # $@ = binaries; prints missing sonames, one per line
    for f in "$@"; do
        ldd "$f" 2>/dev/null | awk '/=> not found/ { print $1 }'
    done | LC_ALL=C sort -u
}

check_runtime() { # $1 = extracted app dir
    glibc_ok || die "Metro needs glibc $GLIBC_MIN or newer (Ubuntu 24.04, Debian 13, a current Arch/CachyOS or Fedora); this system has $(getconf GNU_LIBC_VERSION 2>/dev/null || echo 'no glibc')"
    missing=$(missing_libs "$1/Metro" "$1/metro-server" "$1/ui/Metro")
    [ -z "$missing" ] && return 0
    cmd=$(deps_command)
    printf '%s: missing runtime libraries:\n' "$PROG" >&2
    printf '%s\n' "$missing" | sed 's/^/  /' >&2
    if [ -z "$cmd" ]; then
        die "install GTK 4, WebKitGTK 6.0 and the GStreamer base plugins with your distribution's package manager, then run this again"
    fi
    if [ "$install_deps" != 1 ]; then
        printf '%s: install them with:\n  sudo %s\n' "$PROG" "$cmd" >&2
        die "or run this again with --install-deps to have it run that command for you"
    fi
    run_privileged_install "$cmd"
    missing=$(missing_libs "$1/Metro" "$1/metro-server" "$1/ui/Metro")
    [ -z "$missing" ] || die "still missing after installing packages: $(printf '%s' "$missing" | tr '\n' ' ')"
    say "Runtime libraries are now installed."
}

run_privileged_install() { # $1 = package manager command (fixed list)
    say "Installing Metro's runtime libraries (you may be asked for your password):"
    if [ "$(id -u)" -eq 0 ]; then prefix=; else
        command -v sudo >/dev/null 2>&1 || die "sudo is not available; run as root: $1"
        prefix=sudo
    fi
    say "  ${prefix:+$prefix }$1"
    case "$1" in
        apt-get*) $prefix apt-get update || die "apt-get update failed" ;;
    esac
    # shellcheck disable=SC2086 # fixed word list from deps_command
    $prefix $1 || die "the package manager failed; nothing was installed into $root"
}

# ---------------------------------------------------------------- in-app updates
# The Metro app updates itself too: it journals each step in
# $root/.update.json and locks the installation directory while it mutates it.
JOURNAL=.update.json

has_update_journal() {
    [ -e "$root/$JOURNAL" ] || [ -L "$root/$JOURNAL" ]
}

has_workspace_update_gate() {
    [ -e "$workspace_data/desktop-update.json" ] || [ -L "$workspace_data/desktop-update.json" ]
}

journal_unknown() {
    reason=${1:-"$root/$JOURNAL has an unknown in-app update state."}
    die "[update_journal_unknown] $reason
Stop Metro, its bundled server and any installer. Keep the installation and workspace data, including the update records, live database and backup. Do not remove locks or records, change the current version pointer, or overwrite either database copy.
Use the verified official installer with 'sh install-metro-linux.sh status' for read-only diagnosis, not install, update or uninstall. Contact https://github.com/wnzn/metro/issues with this diagnostic, not private databases or credentials."
}

read_update_journal() {
    [ -f "$root/$JOURNAL" ] && [ ! -L "$root/$JOURNAL" ] &&
        command -v python3 >/dev/null 2>&1 || return 1
    current=$(readlink -- "$root/current" || :)
    python3 - "$root/$JOURNAL" "$current" <<'PY'
import json, os, re, stat, sys

def fields(items):
    result = {}
    for k, v in items:
        if k in result:
            raise ValueError()
        result[k] = v
    return result

def version(s):
    if not isinstance(s, str) or not re.fullmatch(r'(?:0|[1-9][0-9]{0,9})\.(?:0|[1-9][0-9]{0,9})\.(?:0|[1-9][0-9]{0,9})', s):
        raise ValueError()
    values = tuple(map(int, s.split('.')))
    if max(values) > 2**32 - 1:
        raise ValueError()
    return values

try:
    fd = os.open(sys.argv[1], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as f:
        if not stat.S_ISREG(os.fstat(f.fileno()).st_mode):
            raise ValueError()
        raw = f.read(65537)
    if len(raw) > 65536:
        raise ValueError()
    j = json.loads(raw, object_pairs_hook=fields)
    if not isinstance(j, dict) or j.keys() - {'version', 'id', 'from', 'to', 'phase', 'outcome', 'detail', 'local_db_snapshot', 'backup_manifest', 'watchdog_pid', 'reported', 'started_at'}:
        raise ValueError()
    if type(j['version']) is not int or j['version'] not in (1, 2) or not re.fullmatch('[0-9a-f]{16}', j['id']):
        raise ValueError()
    if version(j['from']) >= version(j['to']) or type(j['started_at']) is not int:
        raise ValueError()
    if any(type(j[k]) is not bool for k in ('local_db_snapshot', 'reported') if k in j):
        raise ValueError()
    if j.get('detail') is not None and not isinstance(j['detail'], str):
        raise ValueError()
    pid = j.get('watchdog_pid')
    if pid is not None and (type(pid) is not int or not 0 < pid < 2**32):
        raise ValueError()
    phase = j['phase']
    if phase == 'backing_up' and j['version'] != 2:
        raise ValueError()
    manifest = j.get('backup_manifest')
    if j['version'] == 2 and phase not in ('staged', 'backing_up'):
        if not re.fullmatch('[0-9a-f]{64}', manifest):
            raise ValueError()
    elif manifest is not None:
        raise ValueError()
    outcome = j.get('outcome')
    if phase in ('staged', 'backing_up') and (j.get('local_db_snapshot') or pid is not None):
        raise ValueError()
    if phase == 'committed' and outcome == 'updated':
        expected = {j['to']}
    elif phase == 'failed' and outcome in ('rolled_back', 'not_confirmed', 'install_failed'):
        expected = {j['from']}
    elif phase in ('staged', 'backing_up', 'trial', 'committing', 'rolling_back') and outcome is None:
        expected = {j['from'], j['to']} if phase in ('committing', 'rolling_back') else {j['from']}
    else:
        raise ValueError()
    if sys.argv[2] not in {'versions/' + v for v in expected}:
        raise ValueError()
    if j['version'] == 2 and phase in ('committed', 'failed'):
        phase = 'cleanup'
    print(phase + ' ' + j['to'])
except (ValueError, KeyError, TypeError, OSError, RecursionError):
    sys.exit(1)
PY
}

refuse_during_in_app_update() {
    staged_version=
    if ! has_update_journal; then
        if has_workspace_update_gate; then
            journal_unknown "A workspace update gate exists but this installation's update journal is missing."
        fi
        return 0
    fi
    fields=$(read_update_journal) || journal_unknown
    phase=${fields% *}
    target=${fields#* }
    case "$phase" in
        staged) staged_version=$target ;;
        committed|failed) ;;
        backing_up|cleanup)
            die "Metro is in the middle of updating itself to $target. Let it finish, or close Metro and start it once to complete or undo the update, then run this again" ;;
        trial|committing|rolling_back)
            die "[update_recovery_required] Metro is in the middle of updating itself to $target. Let the active Metro finish. If it stopped, keep the install, update records and both database copies unchanged. Run 'sh install-metro-linux.sh status' for read-only diagnosis and contact https://github.com/wnzn/metro/issues; do not run install, update or uninstall to recover." ;;
        *) journal_unknown ;;
    esac
    # A valid staged or terminal install journal is not proof that the
    # workspace's durable gate has been retired by its verified owner.
    if has_workspace_update_gate; then
        die "[update_recovery_required] A workspace update gate is still present. Leave the records, current pointer and both database copies unchanged. Use 'sh install-metro-linux.sh status' for read-only diagnosis and contact https://github.com/wnzn/metro/issues if Metro cannot verify recovery."
    fi
}

lock_busy() { # why $root/.lock exists
    if [ -f "$root/.lock/owner" ] && grep -q '"by":"metro-desktop"' "$root/.lock/owner" 2>/dev/null; then
        die "Metro is preparing an in-app update right now (it holds $root/.lock). Wait for it to finish, then run this again"
    fi
    die "another install is running (or a previous one crashed: remove $root/.lock and retry)"
}

install_root_has_entries() { # optionally skip the lock we just took
    for root_entry in "$root"/* "$root"/.[!.]* "$root"/..?*; do
        [ "$root_entry" != "${1:-}" ] || continue
        if [ -e "$root_entry" ] || [ -L "$root_entry" ]; then
            return 0
        fi
    done
    return 1
}

lock_install_root() {
    if [ ! -d "$root" ] || [ -L "$root" ]; then
        die "install directory is not a plain directory"
    fi
    if install_root_has_entries && { [ ! -f "$root/$MARK" ] || [ -L "$root/$MARK" ]; }; then
        die "$root was not created by this installer; refusing to change it"
    fi
    [ "$(stat -c '%u' -- "$root")" = "$(id -u)" ] || die "install directory is not owned by you"
    mode=$(stat -c '%a' -- "$root")
    [ "$((0$mode & 022))" -eq 0 ] || die "install directory is writable by others"
    exec 9<"$root"
    flock -n -x 9 || die "another install is running (Metro may be preparing an in-app update right now)"
    owns_install_root || die "the install directory changed"
    compat_take
}

owns_install_root() {
    [ ! -L "$root" ] || return 1
    # Linux sh's -ef compares device/inode with the locked directory fd and fails if either path is missing.
    # shellcheck disable=SC3013
    [ "$root" -ef /proc/self/fd/9 ]
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
        lock_busy
    fi
    compat_new=$(mktemp -d -- "$root/.lock-new.XXXXXX")
    compat_token=metro-lock-v2:${compat_new##*/}
    printf '%s\n' "$compat_token" >"$compat_new/owner.metro-lock-v2"
    mv -nT -- "$compat_new" "$root/.lock"
    if [ -d "$compat_new" ]; then
        rm -- "$compat_new/owner.metro-lock-v2"
        rmdir -- "$compat_new"
        lock_busy
    fi
    trap 'compat_release' EXIT
}

compat_release() {
    if owns_install_root && [ ! -L "$root/.lock" ] &&
        [ ! -L "$compat_owner" ] && [ -f "$compat_owner" ] &&
        [ "$(cat -- "$compat_owner")" = "$compat_token" ]; then
        compat_retire || :
    fi
}

# ---------------------------------------------------------------- install
lock() {
    # Only the new install root is private; parent directories use the umask.
    # shellcheck disable=SC2174
    mkdir -p -m 700 -- "$root"
    lock_install_root
    if [ -e "$root/current" ] || install_root_has_entries "$root/.lock"; then
        if [ ! -f "$root/$MARK" ] || [ -L "$root/$MARK" ]; then
            die "$root exists but was not created by this installer; refusing to change it"
        fi
    fi
    refuse_during_in_app_update
    # On failure before anything was installed, leave no trace behind.
    trap 'if owns_install_root; then
        if [ -n "$old" ] && [ ! -e "$root/versions/$version" ] && [ -e "$old/$version" ]; then
            mv -- "$old/$version" "$root/versions/$version"
        fi
        [ -z "$stage" ] || rm -rf -- "$stage"
        compat_release
        if [ ! -L "$root/current" ] && [ -z "$old" ]; then
            rm -f -- "$root/$MARK"; rmdir -- "$root" 2>/dev/null || :
        fi
    fi' EXIT
    trap 'exit 130' INT TERM
    if [ ! -f "$root/$MARK" ]; then
        printf 'Metro desktop per-user install (managed by %s). Your data is in %s.\n' "$PROG" "$user_data" >"$root/$MARK"
    fi
}

install_app() {
    [ -n "$archive" ] || die "--archive is required"
    [ -f "$archive" ] || die "archive not found: $archive"
    archive=$(absolute "$archive")
    name=${archive##*/}
    case "$name" in
        Metro-*-linux-x64.tar.gz) top=${name%.tar.gz}; version=${top#Metro-}; version=${version%-linux-x64} ;;
        *) die "expected a Metro-<version>-linux-x64.tar.gz archive, got $name" ;;
    esac
    printf '%s' "$version" | grep -Eq '^v?[0-9]+\.[0-9]+\.[0-9]+([-+][A-Za-z0-9][A-Za-z0-9.-]{0,40})?$' ||
        die "unexpected version in archive name: $version"
    stage=
    old=
    lock
    [ "$action" != update ] || [ -L "$root/current" ] || die "Metro is not installed in $root; use install"
    stage=$(mktemp -d -- "$root/.stage.XXXXXX")
    cp -- "$archive" "$stage/$name"
    if [ -z "$sha" ] && [ -z "$checksum_file" ]; then
        if [ -f "$archive.sha256" ]; then checksum_file=$archive.sha256
        elif [ -f "${archive%/*}/SHA256SUMS" ]; then checksum_file=${archive%/*}/SHA256SUMS
        else die "no checksum for $name: pass --sha256 HEX or --checksum-file FILE"
        fi
    fi
    archive=$stage/$name
    verify_archive_checksum "$archive"
    extract_verified "$archive" "$stage" "$top"
    app=$stage/$top
    if [ "$(json_field "$app/RELEASE.json" kind)" != desktop ] ||
        [ "$(json_field "$app/RELEASE.json" target)" != linux-x64 ] ||
        [ "$(json_field "$app/RELEASE.json" version)" != "$version" ]; then
        die "RELEASE.json does not describe a Metro $version linux-x64 desktop package"
    fi
    for f in Metro metro-server ui/Metro; do
        if [ ! -f "$app/$f" ] || [ ! -x "$app/$f" ]; then
            die "package is missing executable $f"
        fi
    done
    check_runtime "$app"
    # Only launchers that read the marker say so in their manifest.
    if [ "$(json_field "$app/RELEASE.json" local_runner)" = installer-marker ]; then
        supports_local_runner=yes
    else
        supports_local_runner=no
    fi

    # Local runner choice: explicit flag, else the previous install's, else on.
    if [ -z "$local_runner" ]; then
        if [ -L "$root/current" ] && [ -e "$root/current/Metro" ]; then
            if [ -f "$root/current/enable-local-runner" ]; then local_runner=yes; else local_runner=no; fi
        else
            local_runner=yes
        fi
    fi
    if [ "$local_runner" = yes ]; then
        : >"$app/enable-local-runner"
        chmod 644 -- "$app/enable-local-runner"
    fi

    owns_install_root || die "the install directory changed"
    [ ! -L "$root/versions" ] || die "versions directory is a symlink"
    mkdir -p -- "$root/versions"
    if [ -e "$root/versions/$version" ]; then
        old=$(mktemp -d -- "$root/.old.XXXXXX")
        mv -- "$root/versions/$version" "$old/"
    fi
    mv -- "$app" "$root/versions/$version"
    ln -sfn -- "versions/$version" "$root/.current.new"
    mv -fT -- "$root/.current.new" "$root/current"
    [ -z "$old" ] || rm -rf -- "$old"
    if [ -n "$staged_version" ] && [ "$staged_version" != "$version" ]; then
        rm -rf -- "$root/versions/$staged_version"
    fi
    # Supersede the validated stage without discarding the previous working install.
    rm -f -- "$root/$JOURNAL"
    if [ "$migrate" = 1 ]; then
        migration=$(mktemp -- "$root/.migrate.XXXXXX")
        printf '%s\n' metro-update-lock-v2 >"$migration"
        mv -fT -- "$migration" "$root/$MARK"
    fi

    if [ "$menu" = 1 ]; then
        if [ -x "$root/versions/$version/install-desktop" ]; then
            "$root/versions/$version/install-desktop" --install >/dev/null ||
                say "warning: could not add the applications-menu entry; run $root/current/install-desktop --install"
        else
            # Archives built before the menu integration (early 0.2.0 candidates).
            say "This package has no applications-menu integration; start Metro with $root/current/Metro"
        fi
    fi
    say "Metro $version is installed in $root/versions/$version"
    say "Start it from your applications menu or run: $root/current/Metro"
    if [ "$local_runner" != yes ]; then
        say "Local runner support is off (reinstall with --local-runner to turn it on)."
    elif [ "$supports_local_runner" = yes ]; then
        say "Local runner support is on. Metro sets up your personal on-device runner after you sign in; it is not attached to any Space until you choose it under Manage Space."
    else
        say "Local runner support was requested, but this Metro build predates it, so no runner will be set up. It takes effect once you update to a build that supports it."
    fi
    say "Your Metro data stays in $user_data."
}

uninstall_app() {
    [ -d "$root" ] || { say "Metro is not installed in $root."; return 0; }
    lock_install_root
    if [ ! -f "$root/$MARK" ] || [ -L "$root/$MARK" ]; then
        die "$root was not created by this installer; refusing to remove it"
    fi
    refuse_during_in_app_update
    if [ -x "$root/current/install-desktop" ]; then
        "$root/current/install-desktop" --uninstall >/dev/null || say "warning: could not remove the applications-menu entry"
    fi
    retired=$(mktemp -d -- "${root%/*}/.metro-uninstall.XXXXXX")
    mv -T -- "$root" "$retired/install"
    rm -rf -- "$retired"
    say "Metro was removed from $root."
    say "Your Metro data was kept in $user_data (delete that folder yourself to remove it)."
}

status() {
    say "Install directory: $root"
    say "Workspace data: $workspace_data"
    if has_workspace_update_gate; then
        say "Workspace update gate present. Leave it and the workspace data intact."
        if ! has_update_journal; then
            journal_unknown "A workspace update gate exists but this installation's update journal is missing."
        fi
    fi
    if has_update_journal; then
        fields=$(read_update_journal) || journal_unknown
        say "Update journal phase: ${fields% *} (read-only diagnosis; does not authorize recovery)"
    fi
    if [ -L "$root/current" ]; then
        target=$(readlink -- "$root/current")
        say "Installed: Metro ${target#versions/} in $root/$target"
        if [ ! -f "$root/current/enable-local-runner" ]; then say "Local runner support: off"
        elif [ "$(json_field "$root/current/RELEASE.json" local_runner)" = installer-marker ]; then say "Local runner support: on"
        else say "Local runner support: requested (this build predates it; no runner is set up)"; fi
    else
        say "Metro is not installed in $root."
    fi
    say "Data: $user_data"
}

case "$action" in
    install|update) install_app ;;
    uninstall) uninstall_app ;;
    status) status ;;
    check-deps)
        [ -L "$root/current" ] || die "Metro is not installed in $root"
        check_runtime "$root/current"
        say "Runtime libraries: OK" ;;
    -h|--help|help) usage ;;
    '') usage >&2; exit 2 ;;
    *) usage >&2; die "unknown command: $action" ;;
esac
