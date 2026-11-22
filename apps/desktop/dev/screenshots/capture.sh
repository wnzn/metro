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
#
# Writes connection.png, room-runner.png, space-members.png, connection-error.png,
# settings-runners.png, settings-space-runner.png, settings-appearance.png,
# and with a model agent-thinking.png, agent-activity-expanded.png, chat-runs.png
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
  METRO_DEV_USERS="$(cat "$sec/dev-users.json")" "${model_env[@]}" \
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
run_app() { # stdin: steps
  docker run --rm -i --init --network host --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -v "$NSH_DIST":/opt/nsh:ro -v "$app":/app -v "$out":/shots -w /app "$image" \
    sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux
}
steps() { URL="$url" SPACE="$sid" SITE="$site_sid" FIXTURE_URL="$fixture_url" SHOTS=/shots AGENT="$agent" node "$here/steps.mjs" "$@"; }

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

echo "== Alice: Settings (runners, model providers, device onboarding, appearance) and a Space's runner"
TOKEN_FILE="$sec/alice" steps alice-settings | run_app
# The connection the steps created exists on the server, its key-less listing came from the fixture.
if [ -n "$fixture_url" ]; then
  api alice GET /v1/providers | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const c=JSON.parse(s).connections;if(!c.some(x=>x.model==="metro-scripted-test-model"&&x.is_default))process.exit(1)})' ||
    { echo "the provider connection was not saved with the listed model" >&2; exit 1; }
fi

echo "== An invalid token is rejected"
steps invalid | run_app

# Tokens only ever lived in $sec; make sure none reached the output or the app.
for f in "$sec/alice" "$sec/bob" "$sec/runner" "$sec/org-runner" "$sec/alice-runner" "$sec/provider.key"; do
  if grep -rqF -- "$(cat "$f")" "$out" "$app" 2>/dev/null; then
    echo "a token was found under $out or $app; refusing to keep the output" >&2
    exit 1
  fi
done
echo "== wrote (agent: ${agent:-none}):"; ls -l "$out"/*.png
[ "$agent" = scripted ] && echo "note: agent replies and the 👀 in these shots come from scripted_model.py (a model double); server, runner and listings are real."
exit 0
