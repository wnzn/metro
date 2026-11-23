#!/bin/sh
# Optional, per-user freedesktop integration for the extracted Linux bundle.
set -eu

id=dev.metro.desktop
data=${XDG_DATA_HOME:-"$HOME/.local/share"}
case "$data" in
    /*) ;;
    *) data="$HOME/.local/share" ;; # The XDG spec ignores relative values.
esac
apps="$data/applications"
theme="$data/icons/hicolor"
entry="$apps/$id.desktop"
png="$theme/256x256/apps/$id.png"
svg="$theme/scalable/apps/$id.svg"

case "${1:-}:$#" in
    --install:1)
        # Preserve trailing newlines long enough to reject control characters.
        case "$0" in */*) parent=${0%/*}/ ;; *) parent=. ;; esac
        here=$(CDPATH='' cd -P -- "$parent" && pwd -P && printf .)
        here=${here%??} # strip pwd's newline and our marker
        if [ ! -x "$here/Metro" ]; then
            printf '%s\n' 'Keep install-desktop next to the extracted Metro executable.' >&2
            exit 1
        fi
        if printf '%s' "$here" | LC_ALL=C grep -q '[[:cntrl:]]' ||
            [ "$(printf '%s' "$here" | wc -l)" -ne 0 ]; then
            printf '%s\n' 'The Metro folder path must not contain control characters.' >&2
            exit 1
        fi
        # Exec uses both Desktop Entry string escaping and its quoting rules.
        # Quoted arguments escape "$\` and %% is a literal percent field code.
        exec_path=$(printf '%s\n' "$here/Metro" |
            sed -e 's/[%]/%%/g' -e 's/["$`\\]/\\&/g' -e 's/\\/\\\\/g')
        install -Dm644 -- "$here/icons/$id.png" "$png"
        install -Dm644 -- "$here/icons/$id.svg" "$svg"
        mkdir -p -- "$apps"
        tmp=$(mktemp -- "$apps/.metro-desktop.XXXXXX")
        trap 'rm -f -- "$tmp"' EXIT
        {
            printf '[Desktop Entry]\nType=Application\nName=Metro\n'
            # GLib resolves argv[0] before expanding %%: use env to exec the
            # absolute launcher path as an argument without involving a shell.
            printf 'Exec=/usr/bin/env "%s"\nIcon=%s\nStartupWMClass=%s\n' "$exec_path" "$id" "$id"
            printf 'Terminal=false\nCategories=Utility;\n'
        } >"$tmp"
        chmod 644 -- "$tmp"
        mv -fT -- "$tmp" "$entry"
        printf 'Metro added to your applications menu (%s).\n' "$entry"
        ;;
    --uninstall:1)
        rm -f -- "$entry" "$png" "$svg"
        printf '%s\n' 'Metro removed from your applications menu. App data was not changed.'
        ;;
    *)
        printf '%s\n' 'Usage: ./install-desktop --install | --uninstall' >&2
        exit 2
        ;;
esac
