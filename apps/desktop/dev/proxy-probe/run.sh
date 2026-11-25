#!/usr/bin/env bash
# Proves what NSH's real fetch does with a proxy configured, using the UI's
# NO_PROXY exactly as the packaged launcher builds it: the person's entries
# (here corp.example, one Tailscale name, and a Unicode look-alike) plus this machine's
# exact hosts (metro_url_policy::loopback_no_proxy_entries, listed in
# crates/url-policy/testdata/host_vectors.json). Loopback and the listed name
# must be reached directly; everything else through the proxy, including
# unlisted Tailscale hosts and ordinary names that merely start like a trusted
# range (100.64.office.example, 127.foo.example).
#
# Everything runs in containers with no network of their own: a Metro-server
# stand-in and a recording proxy (servers.py) on the namespace's 127.0.0.1,
# and NSH's native Linux host on a private Xvfb display, sharing that
# namespace. Names resolve through a private hosts file to 127.0.0.1. With no
# route out, a direct request to a Tailscale address fails at once without
# touching the proxy, while a proxied one shows up in the proxy's log by its
# exact authority. Every URL must produce exactly one result. No token or
# real server is involved.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   apps/desktop/dev/proxy-probe/run.sh
#
# Env: NSH_DIST (default: from `nsh` on PATH). CONTROL=1 uses a NO_PROXY with
# prefix entries instead (.ts.net, *.ts.net., 127., 100.64. ... 100.127.):
# the Tailscale cases and the look-alike names then go direct, which shows the
# probe tells the two apart, and why the launcher uses exact hosts only.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../.." && pwd)"
shots="$repo/apps/desktop/dev/screenshots"
image="metro-desktop-shots:trixie"
py_image="mcr.microsoft.com/playwright:v1.58.2-noble" # any image with python3
if [ -z "${NSH_DIST:-}" ]; then
  nsh_path="$(readlink -f "$(command -v nsh)")" || { echo "nsh not on PATH; set NSH_DIST" >&2; exit 1; }
  NSH_DIST="$(dirname "$(dirname "$nsh_path")")"
fi
[ -x "$NSH_DIST/bin/nsh" ] || { echo "no NSH distribution at $NSH_DIST" >&2; exit 1; }
for i in "$image" "$py_image"; do
  docker image inspect "$i" >/dev/null 2>&1 || { echo "missing image $i" >&2; exit 1; }
done

tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-proxy-probe-XXXXXX")"
net="metro-proxy-probe-$$"
cleanup() { docker rm -f "$net" >/dev/null 2>&1 || true; rm -rf "$tmp"; }
trap cleanup EXIT
chmod 755 "$tmp"
: >"$tmp/proxy.log"; chmod 666 "$tmp/proxy.log"
cp -R "$here/app" "$tmp/app"; mkdir -p "$tmp/app/.nsh"; chmod -R a+rX "$tmp/app"
printf '127.0.0.1 localhost\n::1 localhost\n127.0.0.1 box.tail1234.ts.net other.tail1234.ts.net kbox.ts.net ordinary.example 100.64.office.example 127.foo.example\n' >"$tmp/hosts"

loopback="$(python3 -c 'import json,sys; print(",".join(json.load(open(sys.argv[1]))["launcher_no_proxy_entries"]))' \
  "$repo/crates/url-policy/testdata/host_vectors.json")"
person="corp.example,box.tail1234.ts.net,"$'\u212abox.ts.net'
if [ "${CONTROL:-0}" = 1 ]; then
  prefixes=".ts.net,*.ts.net.,127."
  for b in $(seq 64 127); do prefixes="$prefixes,100.$b."; done
  no_proxy="$person,$loopback,$prefixes"
  ctl=direct
else
  no_proxy="$person,$loopback"
  ctl=proxied
fi

# url expected
cases=(
  "http://127.0.0.1:8787/ direct"
  "http://localhost:8787/ direct"
  "http://[::1]:8787/ direct"
  "http://box.tail1234.ts.net:8787/ direct"
  "http://box.tail1234.ts.net.:8787/ $ctl"
  "http://other.tail1234.ts.net:8787/ $ctl"
  "http://kbox.ts.net:8787/ $ctl"
  "http://100.101.102.103:8787/ $ctl"
  "http://[fd7a:115c:a1e0::9]:8787/ proxied"
  "http://100.64.office.example:8787/ $ctl"
  "http://127.foo.example:8787/ $ctl"
  "http://ordinary.example:8787/ proxied"
  "http://192.168.77.9:8787/ proxied"
)
urls=""; for c in "${cases[@]}"; do urls="$urls ${c%% *}"; done

docker run -d --rm --name "$net" --network none --user "$(id -u):$(id -g)" \
  -v "$here/servers.py:/servers.py:ro" -v "$tmp:/out" "$py_image" \
  python3 /servers.py 8787 3128 /out/proxy.log >/dev/null
sleep 1

steps='await app.waitUntil(() => app.texts().join(" ").includes("DONE"), { timeout: 60000 })
console.log("PROBE-BEGIN\n" + app.texts().join("\n") + "\nPROBE-END")'
printf '%s\n' "$steps" | docker run --rm -i --init --network "container:$net" --user "$(id -u):$(id -g)" \
  -e HOME=/tmp -e DISPLAY_NUM=$((200 + RANDOM % 600)) \
  -e HTTP_PROXY=http://127.0.0.1:3128 -e NO_PROXY="$no_proxy" -e no_proxy="$no_proxy" -e PROBE_URLS="$urls" \
  -v "$tmp/hosts:/etc/hosts:ro" -v "$NSH_DIST:/opt/nsh:ro" -v "$tmp/app:/app:ro" \
  --tmpfs "/app/.nsh:rw,exec,mode=0700,uid=$(id -u),gid=$(id -g)" \
  -v "$shots/with-display.sh:/with-display.sh:ro" -w /app "$image" \
  sh /with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux >"$tmp/nsh.out" 2>&1 || true

python3 "$here/check.py" "$tmp/nsh.out" "$tmp/proxy.log" "${cases[@]}"
