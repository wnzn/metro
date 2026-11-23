#!/bin/sh
# Metro desktop for macOS: per-user install, update and uninstall from a
# prebuilt Metro-<version>-macos-<arm64|x64>.tar.gz into
# ~/Applications/Metro Desktop/, a private folder that holds only Metro.app
# and, when local runner support is on, an empty enable-local-runner file
# (plus Finder's .DS_Store). The installer's own records live in
# ~/Library/Application Support/Metro/Installer. Your Metro data
# (~/Library/Application Support/Metro/Desktop) is never touched.
#
# NOT YET VERIFIED: macOS builds so far are ad hoc signed CI previews, not
# Developer ID signed or notarized. This script never removes quarantine
# attributes or changes Gatekeeper settings; if macOS refuses to open such a
# build, that is expected until a notarized release exists.
set -eu
umask 022

PROG=install-metro-macos
APP_ID=dev.metro.desktop

die() { printf '%s: %s\n' "$PROG" "$*" >&2; exit 1; }
say() { printf '%s\n' "$*"; }

usage() {
    cat <<'EOF'
Usage:
  install-metro-macos.sh install   --archive FILE [--sha256 HEX | --checksum-file FILE] [--no-local-runner | --local-runner]
  install-metro-macos.sh update    --archive FILE [...]
  install-metro-macos.sh uninstall
  install-metro-macos.sh status

Installs Metro.app into ~/Applications/Metro Desktop/ (METRO_INSTALL_DIR
overrides it). Local runner support is on for a new install unless you pass
--no-local-runner; updates keep your previous choice.
EOF
}

absolute() { case "$1" in /*) printf '%s' "$1" ;; *) printf '%s/%s' "$(pwd -P)" "$1" ;; esac; }

case "${HOME:-}" in /*) ;; *) die "HOME must be set to an absolute path" ;; esac
root=${METRO_INSTALL_DIR:-"$HOME/Applications/Metro Desktop"}
case "$root" in /*) ;; *) die "METRO_INSTALL_DIR must be absolute" ;; esac
case "$root" in */) root=${root%/} ;; esac
parent=${root%/*}
user_data="$HOME/Library/Application Support/Metro/Desktop"
state="$HOME/Library/Application Support/Metro/Installer"

action=${1:-}
[ $# -gt 0 ] && shift
archive='' sha='' checksum_file='' local_runner=''
while [ $# -gt 0 ]; do
    case "$1" in
        --archive) [ $# -ge 2 ] || die "--archive needs a file"; archive=$2; shift 2 ;;
        --sha256) [ $# -ge 2 ] || die "--sha256 needs a value"; sha=$2; shift 2 ;;
        --checksum-file) [ $# -ge 2 ] || die "--checksum-file needs a file"; checksum_file=$2; shift 2 ;;
        --no-local-runner) local_runner=no; shift ;;
        --local-runner) local_runner=yes; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; die "unknown option: $1" ;;
    esac
done

sha_of() {
    if command -v shasum >/dev/null 2>&1; then shasum -a 256 -- "$1" | cut -d' ' -f1
    else sha256sum -- "$1" | cut -d' ' -f1; fi
}

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
            else die "no checksum for $name: pass --sha256 HEX or --checksum-file FILE"
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

json_field() { sed -n "s/^  \"$2\": \"\\([^\"\\\\]*\\)\",\\{0,1\\}\$/\\1/p" "$1"; }

extract_verified() { # $1 archive, $2 dest, $3 top folder
    tar -tvzf "$1" >"$2/.list" || die "cannot read $1"
    if awk '{ if (substr($1, 1, 1) != "-") bad = 1 } END { exit bad ? 0 : 1 }' "$2/.list"; then
        die "archive contains something other than regular files; refusing it"
    fi
    tar -tzf "$1" >"$2/.list"
    awk -v top="$3/" 'index($0, top) != 1 || $0 ~ /(^|\/)\.\.(\/|$)/ { bad = 1 } END { exit bad ? 0 : 1 }' "$2/.list" &&
        die "archive entries must all be inside $3/; refusing it"
    rm -f -- "$2/.list"
    tar -xzf "$1" -C "$2" || die "cannot extract $1"
    dir=$2/$3
    [ -f "$dir/RELEASE.json" ] || die "archive has no RELEASE.json manifest"
    manifest_sums "$dir/RELEASE.json" >"$2/.sums" || die "RELEASE.json has no valid file list"
    while IFS= read -r line; do
        h=${line%%"  "*}; p=${line#*"  "}
        [ -f "$dir/$p" ] && [ ! -L "$dir/$p" ] && [ "$(sha_of "$dir/$p")" = "$h" ] ||
            die "files in the archive do not match its RELEASE.json manifest ($p)"
    done <"$2/.sums"
    (cd "$dir" && find . ! -type d ! -type f | grep -q .) && die "archive contains links or special files"
    { cut -c67- "$2/.sums"; echo RELEASE.json; } | LC_ALL=C sort >"$2/.want"
    (cd "$dir" && find . -type f | sed 's|^\./||' | LC_ALL=C sort) >"$2/.have"
    [ "$(cat -- "$2/.want")" = "$(cat -- "$2/.have")" ] || die "archive has files that its RELEASE.json does not list"
    rm -f -- "$2/.sums" "$2/.want" "$2/.have"
}

# The install folder may hold only these; anything else means it is not ours.
only_ours() {
    [ -d "$root" ] || return 0
    for entry in "$root"/* "$root"/.*; do
        case "${entry##*/}" in
            .|..|'*'|'.*'|Metro.app|enable-local-runner|.DS_Store) ;;
            *) return 1 ;;
        esac
    done
    [ ! -e "$root/Metro.app" ] || [ "$(plutil -extract CFBundleIdentifier raw "$root/Metro.app/Contents/Info.plist" 2>/dev/null)" = "$APP_ID" ]
}

running() { # any process started from the installed bundle
    [ -d "$root/Metro.app" ] || return 1
    ps -axo command= | awk -v p="$root/Metro.app/" 'index($0, p) == 1 { f = 1 } END { exit f ? 0 : 1 }'
}

install_app() {
    [ "$(uname -s)" = Darwin ] || die "this installer is for macOS"
    [ -n "$archive" ] || die "--archive is required"
    [ -f "$archive" ] || die "archive not found: $archive"
    archive=$(absolute "$archive"); name=${archive##*/}
    case "$name" in
        Metro-*-macos-arm64.tar.gz) arch=arm64 ;;
        Metro-*-macos-x64.tar.gz) arch=x64 ;;
        *) die "expected a Metro-<version>-macos-arm64.tar.gz or -macos-x64.tar.gz archive, got $name" ;;
    esac
    top=${name%.tar.gz}; version=${top#Metro-}; version=${version%-macos-"$arch"}
    case "$(uname -m)" in
        arm64) [ "$arch" = arm64 ] || die "this Mac has Apple silicon: use the macos-arm64 archive (the x64 build under Rosetta is not supported)" ;;
        x86_64) [ "$arch" = x64 ] || die "this Mac has an Intel processor: use the macos-x64 archive" ;;
        *) die "unsupported Mac architecture: $(uname -m)" ;;
    esac
    os=$(sw_vers -productVersion 2>/dev/null || echo 0)
    [ "${os%%.*}" -ge 14 ] 2>/dev/null || die "Metro needs macOS 14 or newer (this Mac has $os)"
    [ "$action" != update ] || [ -d "$root/Metro.app" ] || die "Metro is not installed in $root; use install"
    verify_archive_checksum "$archive"

    only_ours || die "$root holds files this installer did not put there; refusing to change it"
    running && die "Metro is running from $root; quit it first"
    mkdir -p -- "$parent" "$state"
    lockdir="$parent/.metro-desktop-install.lock"
    mkdir -- "$lockdir" 2>/dev/null || die "another install is running (or crashed: remove '$lockdir')"
    stage=
    trap 'rm -rf -- "$lockdir" ${stage:+"$stage"}' EXIT
    trap 'exit 130' INT TERM
    # Staged beside the install folder (same volume, so the final move is atomic).
    stage=$(mktemp -d -- "$parent/.metro-desktop-stage.XXXXXX")
    extract_verified "$archive" "$stage" "$top"
    app=$stage/$top
    [ "$(json_field "$app/RELEASE.json" kind)" = desktop ] && [ "$(json_field "$app/RELEASE.json" target)" = "macos-$arch" ] &&
        [ "$(json_field "$app/RELEASE.json" version)" = "$version" ] ||
        die "RELEASE.json does not describe a Metro $version macos-$arch desktop package"
    [ -x "$app/Metro.app/Contents/MacOS/Metro" ] || die "package has no Metro.app"
    codesign --verify --deep --strict "$app/Metro.app" 2>/dev/null ||
        die "Metro.app's code signature is invalid or the app was modified; do not install it"
    if [ "$(json_field "$app/RELEASE.json" signing)" = adhoc ]; then
        say "Note: this is an ad hoc signed preview build (not notarized). macOS may refuse to open it; Metro does not ask you to change Gatekeeper."
    fi

    if [ -z "$local_runner" ]; then
        if [ -d "$root/Metro.app" ]; then
            if [ -f "$root/enable-local-runner" ]; then local_runner=yes; else local_runner=no; fi
        else local_runner=yes; fi
    fi

    mkdir -p -- "$root"
    chmod 700 -- "$root"
    old=
    if [ -e "$root/Metro.app" ]; then old=$(mktemp -d -- "$parent/.metro-desktop-old.XXXXXX"); mv -- "$root/Metro.app" "$old/"; fi
    mv -- "$app/Metro.app" "$root/Metro.app"
    # Release notes, licenses and the manifest go to the installer's records.
    rm -rf -- "$state/licenses" "$state/RELEASE.json" "$state/START.txt"
    for f in RELEASE.json START.txt licenses; do [ -e "$app/$f" ] && mv -- "$app/$f" "$state/$f"; done
    printf '%s\n' "$root" >"$state/install-dir"
    # The marker lives beside the sealed bundle, never inside it.
    if [ "$local_runner" = yes ]; then
        rm -f -- "$root/enable-local-runner"; : >"$root/enable-local-runner"; chmod 644 -- "$root/enable-local-runner"
    else
        rm -f -- "$root/enable-local-runner"
    fi
    [ -z "$old" ] || rm -rf -- "$old"
    say "Metro $version is installed: $root/Metro.app"
    if [ "$local_runner" != yes ]; then
        say "Local runner support is off (reinstall with --local-runner to turn it on)."
    elif [ "$(json_field "$state/RELEASE.json" local_runner)" = installer-marker ]; then
        say "Local runner support is on. Metro sets up your personal on-device runner after you sign in; it is not attached to any Space until you choose it under Manage Space."
    else
        say "Local runner support was requested, but this Metro build predates it, so no runner will be set up."
    fi
    say "Your Metro data stays in $user_data."
}

uninstall_app() {
    [ -d "$root" ] || { say "Metro is not installed in $root."; rm -rf -- "$state"; return 0; }
    only_ours || die "$root holds files this installer did not put there; refusing to remove it"
    running && die "Metro is running from $root; quit it first"
    rm -rf -- "$root" "$state"
    say "Metro was removed from $root."
    say "Your Metro data was kept in $user_data."
}

case "$action" in
    install|update) install_app ;;
    uninstall) uninstall_app ;;
    status)
        if [ -d "$root/Metro.app" ] && [ -f "$state/RELEASE.json" ]; then say "Installed: Metro $(json_field "$state/RELEASE.json" version) in $root"
        else say "Metro is not installed in $root."; fi
        if [ ! -f "$root/enable-local-runner" ]; then say "Local runner support: off"
        elif [ "$(json_field "$state/RELEASE.json" local_runner 2>/dev/null)" = installer-marker ]; then say "Local runner support: on"
        else say "Local runner support: requested (this build predates it; no runner is set up)"; fi ;;
    -h|--help|help) usage ;;
    '') usage >&2; exit 2 ;;
    *) usage >&2; die "unknown command: $action" ;;
esac
