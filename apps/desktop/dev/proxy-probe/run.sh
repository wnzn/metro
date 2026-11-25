#!/usr/bin/env bash
# Proves what NSH's real fetch does with a proxy configured, using the UI's
# NO_PROXY exactly as the packaged launcher builds it (the person's entries
# plus metro_url_policy::direct_no_proxy_entries, listed in
# crates/url-policy/testdata/host_vectors.json): loopback and Tailscale
# destinations must be reached directly, any other one through the proxy.
#
# Everything runs in containers with no network of their own: a Metro-server
# stand-in and a recording proxy (servers.py) on the namespace's 127.0.0.1,
# and NSH's native Linux host on a private Xvfb display, sharing that
# namespace. Names resolve through a private hosts file to 127.0.0.1. With no
# route out, a direct request to a Tailscale address fails at once without
# touching the proxy, while a proxied one shows up in the proxy's log. No
# token or real server is involved.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   apps/desktop/dev/proxy-probe/run.sh
#
# Env: NSH_DIST (default: from `nsh` on PATH). CONTROL=1 uses the launcher's
# NO_PROXY from before the Tailscale entries (loopback only): the Tailscale
# cases are then expected to go through the proxy, which shows the probe
# tells the two apart.
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
printf '127.0.0.1 localhost\n::1 localhost\n127.0.0.1 box.tail1234.ts.net ordinary.example\n' >"$tmp/hosts"

if [ "${CONTROL:-0}" = 1 ]; then
  no_proxy="corp.example,127.0.0.1,localhost,::1"
else
no_proxy="corp.example,$(python3 -c 'import json,sys; print(",".join(json.load(open(sys.argv[1]))["launcher_no_proxy_entries"]))' \
  "$repo/crates/url-policy/testdata/host_vectors.json")"
fi

# url expected
cases=(
  "http://box.tail1234.ts.net:8787/ $( [ "${CONTROL:-0}" = 1 ] && echo proxied || echo direct )"
  "http://box.tail1234.ts.net.:8787/ $( [ "${CONTROL:-0}" = 1 ] && echo proxied || echo direct )"
  "http://127.0.0.1:8787/ direct"
  "http://[::1]:8787/ $( [ "${CONTROL:-0}" = 1 ] && echo proxied || echo direct )"
  "http://100.101.102.103:8787/ $( [ "${CONTROL:-0}" = 1 ] && echo proxied || echo direct )"
  "http://[fd7a:115c:a1e0::9]:8787/ $( [ "${CONTROL:-0}" = 1 ] && echo proxied || echo direct )"
  "http://ordinary.example:8787/ proxied"
  "http://192.168.77.9:8787/ proxied"
  "http://[::ffff:6464:6464]:8787/ proxied"
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

python3 - "$tmp/nsh.out" "$tmp/proxy.log" "${cases[@]}" <<'EOF'
import sys
out = open(sys.argv[1]).read()
log = open(sys.argv[2]).read()
if "PROBE-BEGIN" not in out:
    print(out[-3000:]); sys.exit("the probe app did not finish")
results = {}
for line in out.split("PROBE-BEGIN", 1)[1].split("PROBE-END", 1)[0].splitlines():
    if " => " in line:
        u, r = line.split(" => ", 1)
        results[u.strip()] = r.strip()
failed = 0
for case in sys.argv[3:]:
    url, want = case.rsplit(" ", 1)
    host = url.split("//", 1)[1].split("/", 1)[0]
    bare = host.rsplit(":", 1)[0]
    through_proxy = any(bare.strip("[]").rstrip(".") in l for l in log.splitlines())
    got = "proxied" if through_proxy else "direct"
    ok = got == want and (want == "proxied" or not results.get(url, "").startswith("ERROR TypeError: Network request failed: CONNECT proxy failed"))
    failed += not ok
    print(f"{'ok  ' if ok else 'FAIL'} {url:42} expected {want:8} got {got:8} | {results.get(url, '(no result)')[:90]}")
print(f"\n{len(sys.argv) - 3 - failed} passed, {failed} failed")
sys.exit(1 if failed else 0)
EOF
