#!/usr/bin/env bash
# Live screenshots of the desktop app in NSH's native Linux (GTK 4) host,
# against the real metro-server and metro-runner on loopback.
#
# Everything is throwaway and removed on exit: a temp directory with the
# database, the runner's workspace, logs, and the secrets (mode-600 files:
# two dev users' tokens and the Space's runner token). Tokens are read from
# those files only: never printed, never in argv, never typed while a
# screenshot is taken. The GTK window runs on a private Xvfb display inside a
# container, so the developer's desktop is not touched.
#
# `nsh test` writes the steps it was fed, typed tokens included, to
# <app>/.nsh/test/. So the app runs from a staged copy in the temp directory,
# mounted read-only, with a tmpfs at /app/.nsh that dies with the container;
# the checkout is never mounted. NSH's output is redacted before it is shown.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   apps/desktop/dev/screenshots/capture.sh [OUT_DIR]       # default: docs/screenshots
#
# Env:
#   NSH_DIST   NSH distribution dir (bin/nsh, runtime/linux); default: from `nsh` on PATH
#   METRO_BIN  dir with metro-server and metro-runner; default: target/debug
#   MODEL_ENV_FILE  optional mode-600 file of METRO_MODEL* lines: use a real model
#              for the agent shots.
#   SCRIPTED_MODEL  without MODEL_ENV_FILE, 1 (default) runs scripted_model.py, a
#              deterministic local OpenAI-compatible double of the MODEL only
#              (server, Rig, tool call, runner and listing stay real); 0 skips
#              the agent shots so nothing calls a model.
#   THEME      dark (default) or light: every shot after connecting in that
#              appearance (connection*.png are taken in dark runs only).
#
# Writes connection.png, room-runner.png, space-members.png, connection-error.png,
# settings-runners.png, settings-space-runner.png, settings-appearance.png,
# team-room.png, create-space.png, create-room.png, recipient-picker.png,
# reactions.png, profile.png, compact-profile.png, compact-profile-edited.png
# and compact-profile-saved.png (780 wide: Save pressed without scrolling,
# checked on the server), provider-dropdown.png, server-api-profile.png,
# appearance-preview.png, appearance-preview-end.png,
# and with a model agent-thinking.png, agent-activity-expanded.png, chat-runs.png,
# chat-narrow.png (860 wide), chat-dispatch.png and chat-narrow-runs.png (where
# the app has a collapsible task form and a narrow-window runs toggle)
# and thread.png; with the scripted model also
# settings-providers.png and settings-device-onboarding.png (its keyless
# /v1/models is the allowlisted endpoint), into OUT_DIR.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
app="$(cd "$here/../.." && pwd)"
repo="$(cd "$app/../.." && pwd)"
out_arg="${1:-$repo/docs/screenshots}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"
bin="${METRO_BIN:-$repo/target/debug}"
image="metro-desktop-shots:trixie"
if [ -z "${NSH_DIST:-}" ]; then
  nsh_path="$(readlink -f "$(command -v nsh)")" || { echo "nsh not on PATH; set NSH_DIST" >&2; exit 1; }
  NSH_DIST="$(dirname "$(dirname "$nsh_path")")"
fi
[ -x "$NSH_DIST/bin/nsh" ] && [ -x "$NSH_DIST/runtime/linux/nsh-linux-host" ] || { echo "no NSH Linux distribution at $NSH_DIST" >&2; exit 1; }
for b in metro-server metro-runner; do [ -x "$bin/$b" ] || { echo "missing $bin/$b (cargo build)" >&2; exit 1; }; done
docker image inspect "$image" >/dev/null 2>&1 || { echo "build the image first: docker build -t $image $here" >&2; exit 1; }
for tool in node openssl curl python3; do command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }; done

theme="${THEME:-dark}"
case "$theme" in dark|light) ;; *) echo "THEME must be dark or light" >&2; exit 1 ;; esac

umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-shots-XXXXXX")"
sec="$tmp/secrets"; mkdir -m 700 "$sec"
pids=()
cleanup() {
  for p in "${pids[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

# The app as the container sees it: a copy without .nsh, target or node_modules, read-only.
stage="$tmp/app"; mkdir -p "$stage/.nsh" # .nsh: only the tmpfs mountpoint
tar -C "$app" --exclude=.nsh --exclude=target --exclude=node_modules -cf - . | tar -C "$stage" -xf - # at any depth
chmod -R a+rX,go-w "$stage"

# --- secrets (files only) ------------------------------------------------
openssl rand -hex 24 >"$sec/alice"
openssl rand -hex 24 >"$sec/bob"
for u in alice bob; do printf 'Authorization: Bearer %s\n' "$(cat "$sec/$u")" >"$sec/$u.hdr"; done
python3 - "$sec" >"$sec/dev-users.json" <<'PY'
import json, sys, pathlib
d = pathlib.Path(sys.argv[1])
print(json.dumps({u: (d / u).read_text().strip() for u in ("alice", "bob")}))
PY

port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
api() { # api <user> <method> <path> [json] — prints the response body
  local user="$1" method="$2" path="$3" body="${4:-}"
  if [ -n "$body" ]; then
    curl -fsS -X "$method" -H @"$sec/$user.hdr" -H 'Content-Type: application/json' --data-binary "$body" "$url$path"
  else
    curl -fsS -X "$method" -H @"$sec/$user.hdr" "$url$path"
  fi
}

# --- the runner's workspace: small and harmless ---------------------------
ws="$tmp/workspace"; mkdir -p "$ws/src" "$ws/docs"
printf '# Acme demo workspace\n' >"$ws/README.md"
printf 'fn main() {}\n' >"$ws/src/main.rs"
printf 'notes\n' >"$ws/docs/notes.md"
printf '[package]\nname = "acme"\n' >"$ws/Cargo.toml"

# --- server ---------------------------------------------------------------
agent=""
model_env=()
fixture_url=""
openssl rand -base64 32 >"$sec/provider.key" # the provider master key (a file, never argv)
if [ -n "${MODEL_ENV_FILE:-}" ]; then
  [ "$(stat -c %a "$MODEL_ENV_FILE")" = 600 ] || { echo "MODEL_ENV_FILE must be mode 600" >&2; exit 1; }
  while IFS= read -r line; do
    case "$line" in METRO_MODEL=*|METRO_MODEL_BASE_URL=*|METRO_MODEL_API_KEY=*) model_env+=("$line") ;; esac
  done <"$MODEL_ENV_FILE"
  agent=real
elif [ "${SCRIPTED_MODEL:-1}" = 1 ]; then
  python3 "$here/scripted_model.py" "$tmp/model.port" "${MODEL_HOLD_SECONDS:-15}" >"$tmp/model.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 50); do [ -s "$tmp/model.port" ] && break; sleep 0.1; done
  [ -s "$tmp/model.port" ] || { echo "scripted model did not start" >&2; exit 1; }
  fixture_url="http://127.0.0.1:$(cat "$tmp/model.port")/v1"
  model_env=(METRO_MODEL=metro-scripted-test-model "METRO_MODEL_BASE_URL=$fixture_url" METRO_MODEL_API_KEY=local-not-a-real-key
    "METRO_PROVIDER_ALLOWED_BASE_URLS=[\"$fixture_url\"]")
  agent=scripted
fi
env -i PATH=/usr/bin:/bin HOME="$tmp" \
  METRO_BIND="127.0.0.1:$port" METRO_DB="$tmp/metro.sqlite" \
  METRO_DEV_USERS_FILE="$sec/dev-users.json" "${model_env[@]}" \
  METRO_ORG_ADMINS='["alice"]' METRO_PROVIDER_KEY_FILE="$sec/provider.key" \
  "$bin/metro-server" >"$tmp/server.log" 2>&1 &
pids+=($!)
for _ in $(seq 150); do curl -fs -o /dev/null "$url/healthz" && break; sleep 0.1; done
curl -fs -o /dev/null "$url/healthz" || { echo "server did not start" >&2; tail -20 "$tmp/server.log" >&2; exit 1; }

# --- a Space owned by Alice, Bob as member, its own runner -----------------
space_json="$(api alice POST /v1/spaces '{"name":"Acme"}')"
sid="$(node -e 'process.stdout.write(String(JSON.parse(process.argv[1]).id))' "$space_json")"
api alice POST "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
api alice POST "/v1/spaces/$sid/runner-token" '{}' |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>process.stdout.write(JSON.parse(s).token))' >"$sec/runner"
env -i PATH=/usr/bin:/bin HOME="$tmp" METRO_RUNNER_TOKEN="$(cat "$sec/runner")" \
  METRO_SERVER_URL="$url" METRO_WORKSPACE="$ws" \
  "$bin/metro-runner" >"$tmp/runner.log" 2>&1 &
pids+=($!)

# --- Settings: two named runners, and a Space the organization runner serves
field() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{let v=JSON.parse(s);for(const k of process.argv[1].split("."))v=v[k];process.stdout.write(String(v))})' "$1"; }
api alice POST /v1/runners '{"name":"Shared build runner","visibility":"organization"}' >"$sec/org-runner.json"
field token <"$sec/org-runner.json" >"$sec/org-runner"
org_runner="$(field runner.id <"$sec/org-runner.json")"
api alice POST /v1/runners '{"name":"Alice workstation","visibility":"personal"}' | field token >"$sec/alice-runner"
rm -f "$sec/org-runner.json"
site_sid="$(api alice POST /v1/spaces '{"name":"Website"}' | field id)"
api alice PUT "/v1/spaces/$site_sid/runner" "{\"runner_id\":\"$org_runner\"}" >/dev/null
mkdir -p "$tmp/registered"
env -i PATH=/usr/bin:/bin HOME="$tmp" METRO_RUNNER_TOKEN="$(cat "$sec/org-runner")" \
  METRO_SERVER_URL="$url" METRO_WORKSPACE="$tmp/registered" \
  "$bin/metro-runner" >"$tmp/registered-runner.log" 2>&1 &
pids+=($!)

# --- the app, natively, in the container ----------------------------------
# Prints NSH's output with every secret in $sec replaced, never the raw text.
redact() {
  python3 -c '
import pathlib, sys
secrets = [p.read_text().strip() for p in pathlib.Path(sys.argv[1]).iterdir() if p.is_file() and p.suffix != ".hdr"]
secrets = sorted({s for s in secrets if len(s) >= 8}, key=len, reverse=True)
for line in sys.stdin:
    for s in secrets:
        line = line.replace(s, "[redacted]")
    sys.stdout.write(line)
' "$sec"
}
run_app() { # stdin: steps
  # A private, uncommon X display per run: --network host shares abstract X sockets.
  local display=$((200 + RANDOM % 600))
  docker run --rm -i --init --network host --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -e DISPLAY_NUM="$display" -e SCREEN="${SCREEN:-1440x1050x24}" \
    -v "$NSH_DIST":/opt/nsh:ro -v "$stage":/app:ro \
    --tmpfs "/app/.nsh:rw,exec,mode=0700,uid=$(id -u),gid=$(id -g)" \
    -v "$out":/shots -w /app "$image" \
    sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux 2>&1 | redact # pipefail: a failed run fails the script
}
profile_name="Alice Rivera" profile_bio="Runs the Acme workspace." # typed by the tour at 780 wide
steps() { PROFILE_NAME="$profile_name" PROFILE_BIO="$profile_bio" URL="$url" SPACE="$sid" SITE="$site_sid" FIXTURE_URL="$fixture_url" SHOTS=/shots AGENT="$agent" THEME="$theme" node "$here/steps.mjs" "$@"; }

echo "== Bob mentions Alice from his desktop app"
TOKEN_FILE="$sec/bob" steps bob | run_app
event_id="$(api alice GET "/v1/spaces/$sid/rooms/lobby/events?after=0" |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const e=JSON.parse(s).events.find(e=>e.actor==="bob"&&e.kind==="message");process.stdout.write(e?String(e.id):"")})')"
[ -n "$event_id" ] || { echo "Bob's message is not on the server" >&2; exit 1; }

echo "== Alice: connect screen, reaction, message, Demo, members"
TOKEN_FILE="$sec/alice" EVENT_ID="$event_id" steps alice | run_app

if [ -n "$agent" ]; then
  echo "== Alice asks @agent about the workspace: one turn, live then expanded ($agent model)"
  TOKEN_FILE="$sec/alice" steps alice-activity | run_app
  echo "== Alice asks @agent, then the Chat's run controls ($agent model)"
  TOKEN_FILE="$sec/alice" steps alice-chat | run_app
  sleep "${MODEL_HOLD_SECONDS:-15}" # let the child run finish
  thread_id="$(api alice GET "/v1/spaces/$sid/rooms" |
    node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const r=JSON.parse(s).rooms.find(r=>r.name==="workspace-audit"&&r.parent_room_id);process.stdout.write(r?String(r.id):"")})')"
  [ -n "$thread_id" ] || { echo "no delegated thread was created" >&2; exit 1; }
  echo "== Alice opens the delegated thread"
  TOKEN_FILE="$sec/alice" THREAD_ID="$thread_id" steps alice-agent | run_app
fi

echo "== Alice: Settings (runners, model providers, device onboarding)"
TOKEN_FILE="$sec/alice" steps alice-settings | run_app
# The connection the steps created exists on the server, its key-less listing came from the fixture.
if [ -n "$fixture_url" ]; then
  api alice GET /v1/providers | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const c=JSON.parse(s).connections;if(!c.some(x=>x.model==="metro-scripted-test-model"&&x.is_default)){console.error(JSON.stringify(c.map(x=>({name:x.name,provider_id:x.provider_id,model:x.model,is_default:x.is_default,capability:x.capability}))));process.exit(1)}})' ||
    { echo "the provider connection was not saved with the listed model" >&2; exit 1; }
fi

echo "== Alice: appearance and a Space's runner"
TOKEN_FILE="$sec/alice" steps space-settings | run_app

echo "== Alice: room popovers and compact forms"
TOKEN_FILE="$sec/alice" EVENT_ID="$event_id" steps tour | run_app

echo "== Alice: profile (also at 780 wide, saved), provider dropdown, appearance preview"
TOKEN_FILE="$sec/alice" steps tour-settings | run_app
# The compact Save really saved: the server has the name and bio typed at 780 wide.
api alice GET /v1/me/profile | PROFILE_NAME="$profile_name" PROFILE_BIO="$profile_bio" node -e '
let s = ""; process.stdin.on("data", (d) => (s += d)).on("end", () => {
  const p = JSON.parse(s), want = { display_name: process.env.PROFILE_NAME, bio: process.env.PROFILE_BIO };
  const got = { display_name: p.display_name, bio: p.bio };
  if (got.display_name !== want.display_name || got.bio !== want.bio) { console.error("profile not saved:", JSON.stringify(got)); process.exit(1); }
  console.log("profile saved at 780 wide:", JSON.stringify(got));
});' || { echo "the compact profile save did not reach the server" >&2; exit 1; }

echo "== An invalid token is rejected"
steps invalid | run_app

# Tokens only ever lived in $sec; make sure none reached the output or the app.
for f in "$sec/alice" "$sec/bob" "$sec/runner" "$sec/org-runner" "$sec/alice-runner" "$sec/provider.key"; do
  if grep -rqF -f "$f" -- "$out" "$app" "$stage" 2>/dev/null; then # -f: the token never reaches argv
    echo "a token was found under $out, $app or the staged app; refusing to keep the output" >&2
    exit 1
  fi
done
echo "== wrote (agent: ${agent:-none}, theme: $theme):"; ls -l "$out"/*.png
[ "$agent" = scripted ] && echo "note: agent replies and the 👀 in these shots come from scripted_model.py (a model double); server, runner and listings are real."
exit 0
