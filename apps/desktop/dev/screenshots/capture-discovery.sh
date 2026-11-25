#!/usr/bin/env bash
# Native Linux screenshots of Search (Conversations, reranked by System One)
# and Scheduled tasks, against the real metro-server on loopback with local
# doubles only: search_fixture.py (keyless embeddings and System One on
# 127.0.0.1, word overlap, not a semantic model) and scripted_model.py (the
# chat model the fired scheduled task's agent turn uses). Nothing here
# reaches a real vendor.
#
# Everything is throwaway and removed on exit, on failure too: a temp
# directory with the database, logs and the secrets (mode-600 files: two dev
# users' tokens and the provider master key). Tokens are read from those
# files only: never printed, never in argv, never on screen when a
# screenshot is taken.
#
# `nsh test` writes the steps it was fed, typed tokens included, to
# <app>/.nsh/test/. So the app runs from a staged copy in the temp directory,
# mounted read-only, with a tmpfs at /app/.nsh that dies with the container;
# the checkout is never mounted. NSH's output is redacted before it is shown.
#
#   docker build -t metro-desktop-shots:trixie apps/desktop/dev/screenshots
#   apps/desktop/dev/screenshots/capture-discovery.sh [OUT_DIR]   # default: docs/screenshots
#
# Env: NSH_DIST (default: from `nsh` on PATH), METRO_BIN (default: target/debug).
# Writes semantic-search.png and scheduled-tasks.png into OUT_DIR.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
app="$(cd "$here/../.." && pwd)"
repo="$(cd "$app/../.." && pwd)"
out_arg="${1:-$repo/docs/screenshots}"
mkdir -p "$out_arg"
out="$(cd "$out_arg" && pwd)"
bin="$(cd "${METRO_BIN:-$repo/target/debug}" && pwd)"
image="metro-desktop-shots:trixie"
if [ -z "${NSH_DIST:-}" ]; then
  nsh_path="$(readlink -f "$(command -v nsh)")" || { echo "nsh not on PATH; set NSH_DIST" >&2; exit 1; }
  NSH_DIST="$(dirname "$(dirname "$nsh_path")")"
fi
[ -x "$NSH_DIST/bin/nsh" ] && [ -x "$NSH_DIST/runtime/linux/nsh-linux-host" ] || { echo "no NSH Linux distribution at $NSH_DIST" >&2; exit 1; }
[ -x "$bin/metro-server" ] || { echo "missing $bin/metro-server (cargo build)" >&2; exit 1; }
docker image inspect "$image" >/dev/null 2>&1 || { echo "build the image first: docker build -t $image $here" >&2; exit 1; }
for tool in node openssl curl python3; do command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }; done

umask 077
tmp="$(mktemp -d "${TMPDIR:-/tmp}/metro-discovery-shots-XXXXXX")"
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
openssl rand -base64 32 >"$sec/provider.key" # the provider master key (a file, never argv)

# --- the doubles: typed endpoints and the chat model ----------------------
python3 -I "$here/search_fixture.py" "$tmp/typed.port" >"$tmp/typed.log" 2>&1 &
pids+=($!)
python3 -I "$here/scripted_model.py" "$tmp/model.port" 0 >"$tmp/model.log" 2>&1 &
pids+=($!)
for _ in $(seq 50); do [ -s "$tmp/typed.port" ] && [ -s "$tmp/model.port" ] && break; sleep 0.1; done
[ -s "$tmp/typed.port" ] && [ -s "$tmp/model.port" ] || { echo "a fixture did not start" >&2; exit 1; }
typed="http://127.0.0.1:$(cat "$tmp/typed.port")/v1"
model="http://127.0.0.1:$(cat "$tmp/model.port")/v1"

# --- server ---------------------------------------------------------------
port="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')"
url="http://127.0.0.1:$port"
env -i PATH=/usr/bin:/bin HOME="$tmp" METRO_BIND="127.0.0.1:$port" METRO_DB="$tmp/metro.sqlite" \
  METRO_DEV_USERS_FILE="$sec/dev-users.json" METRO_ORG_ADMINS='["alice"]' \
  METRO_MODEL=metro-scripted-test-model METRO_MODEL_BASE_URL="$model" METRO_MODEL_API_KEY=local-not-a-real-key \
  METRO_PROVIDER_ALLOWED_BASE_URLS="[\"$typed\",\"$model\"]" METRO_PROVIDER_KEY_FILE="$sec/provider.key" \
  METRO_SCHEDULE_TICK_SECS=2 \
  "$bin/metro-server" >"$tmp/server.log" 2>&1 &
pids+=($!)
for _ in $(seq 150); do curl -fs -o /dev/null "$url/healthz" && break; sleep 0.1; done
curl -fs -o /dev/null "$url/healthz" || { echo "server did not start" >&2; tail -20 "$tmp/server.log" >&2; exit 1; }

api() { # api <user> <method> <path> [json] — prints the response body
  local user="$1" method="$2" path="$3" body="${4:-}"
  if [ -n "$body" ]; then
    curl -fsS -X "$method" -H @"$sec/$user.hdr" -H 'Content-Type: application/json' --data-binary "$body" "$url$path"
  else
    curl -fsS -X "$method" -H @"$sec/$user.hdr" "$url$path"
  fi
}
field() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{let v=JSON.parse(s);for(const k of process.argv[1].split("."))v=v[k];process.stdout.write(String(v))})' "$1"; }
quote() { node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' "$1"; }

# --- a Space with a conversation, Alice's personal Chat, and Bob's own Space
sid="$(api alice POST /v1/spaces '{"name":"Acme"}' | field id)"
api alice POST "/v1/spaces/$sid/members" '{"user_id":"bob"}' >/dev/null
rel="$(api alice POST "/v1/spaces/$sid/rooms" '{"name":"releases"}' | field id)"
other="$(api bob POST /v1/spaces '{"name":"Bob private"}' | field id)" # Alice isn't a member: never in her results
say() { api "$1" POST "$2" "{\"text\":$(quote "$3"),\"invoke_agent\":false}" >/dev/null; }
say bob "/v1/spaces/$sid/rooms/lobby/messages" "Deploy checklist for Friday: tag the release, build the images, run the smoke tests, then announce it."
say alice "/v1/spaces/$sid/rooms/lobby/messages" "Lunch is at noon in the big room."
say alice "/v1/spaces/$sid/rooms/$rel/messages" "Release 0.4 deploy is blocked until the smoke tests pass on staging."
say bob "/v1/spaces/$sid/rooms/$rel/messages" "Rollback plan: redeploy the previous image tag if the smoke tests fail."
say alice /v1/chat/messages "Note to self: write the deploy checklist into the wiki before Friday."
say bob "/v1/spaces/$other/rooms/lobby/messages" "Bob's private deploy checklist, only in his own Space."

# --- Alice's typed connections on the keyless fixture, chosen for search ----
idof() { node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const v=JSON.parse(s);process.stdout.write(String((v.connection??v).id))})'; }
emb_id="$(api alice POST /v1/providers "{\"name\":\"Local embeddings fixture\",\"provider_id\":\"custom-embeddings\",\"base_url\":\"$typed\",\"model\":\"fixture-embed\"}" | idof)"
s1_id="$(api alice POST /v1/providers "{\"name\":\"Local System One fixture\",\"provider_id\":\"custom-systemone\",\"base_url\":\"$typed\",\"model\":\"fixture-system-one\"}" | idof)"
api alice GET /v1/model-routing |
  node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const v=JSON.parse(s);v.embedding_id=process.argv[1];v.classifier_id=process.argv[2];process.stdout.write(JSON.stringify(v))})' "$emb_id" "$s1_id" >"$tmp/routing.json"
api alice PUT /v1/model-routing "$(cat "$tmp/routing.json")" >/dev/null
indexed=""
for _ in $(seq 20); do
  indexed="$(api alice POST /v1/search/index '{"scope":"all"}' | field done)"
  [ "$indexed" = true ] && break
done
[ "$indexed" = true ] || { echo "the search index did not finish" >&2; exit 1; }

# --- scheduled tasks: daily, weekdays, paused, and one that fires now ------
soon="$(python3 -c 'import datetime as d; print((d.datetime.now(d.timezone.utc)+d.timedelta(seconds=8)).strftime("%Y-%m-%dT%H:%M:%SZ"))')"
task() { api alice POST /v1/schedules "$1" | field id; }
task "{\"name\":\"Morning summary\",\"prompt\":\"Summarise yesterday's messages in this room and list open questions.\",\"space_id\":\"$sid\",\"room_id\":\"lobby\",\"schedule\":{\"kind\":\"cron\",\"expression\":\"0 9 * * *\",\"timezone\":\"Asia/Manila\"},\"enabled\":true}" >/dev/null
task "{\"name\":\"Standup nudge\",\"prompt\":\"Remind me to post my standup notes.\",\"space_id\":null,\"room_id\":null,\"schedule\":{\"kind\":\"cron\",\"expression\":\"30 8 * * 1-5\",\"timezone\":\"Asia/Manila\"},\"enabled\":true}" >/dev/null
fired="$(task "{\"name\":\"Release check\",\"prompt\":\"Check whether the release smoke tests have passed.\",\"space_id\":\"$sid\",\"room_id\":\"$rel\",\"schedule\":{\"kind\":\"once\",\"at\":\"$soon\"},\"enabled\":true}")"
task "{\"name\":\"Friday retro\",\"prompt\":\"Collect three things that went well this week.\",\"space_id\":null,\"room_id\":null,\"schedule\":{\"kind\":\"cron\",\"expression\":\"0 16 * * fri\",\"timezone\":\"Europe/Berlin\"},\"enabled\":false}" >/dev/null
status=""
for _ in $(seq 60); do
  status="$(api alice GET "/v1/schedules/$fired/history" | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>{const r=JSON.parse(s).runs;process.stdout.write(r.length?r[0].status:"")})')"
  case "$status" in dispatched|error|unknown|revoked|missed|interrupted|cancelled) break ;; esac
  sleep 1
done
[ "$status" = dispatched ] || { echo "the one-time task did not dispatch (status: ${status:-none})" >&2; exit 1; }
sleep 3 # its scripted agent turn

# --- the app, natively, in the container ----------------------------------
# Prints NSH's output with every secret in $sec replaced, never the raw text.
redact() {
  python3 -c '
import pathlib, sys
secrets = [p.read_text().strip() for p in pathlib.Path(sys.argv[1]).iterdir() if p.is_file() and p.suffix not in (".hdr", ".json")]
secrets = sorted({s for s in secrets if len(s) >= 8}, key=len, reverse=True)
for line in sys.stdin:
    for s in secrets:
        line = line.replace(s, "[redacted]")
    sys.stdout.write(line)
' "$sec"
}
# A private, uncommon X display per run: --network host shares abstract X sockets.
TOKEN_FILE="$sec/alice" URL="$url" SHOTS=/shots node "$here/discovery-steps.mjs" alice |
  docker run --rm -i --init --network host --user "$(id -u):$(id -g)" -e HOME=/tmp \
    -e DISPLAY_NUM=$((200 + RANDOM % 600)) -e SCREEN=1440x1050x24 \
    -v "$NSH_DIST":/opt/nsh:ro -v "$stage":/app:ro \
    --tmpfs "/app/.nsh:rw,exec,mode=0700,uid=$(id -u),gid=$(id -g)" \
    -v "$out":/shots -w /app "$image" \
    sh /app/dev/screenshots/with-display.sh /opt/nsh/bin/nsh test App.tsx --target linux 2>&1 | redact # pipefail: a failed run fails the script

# The badge says "Reranked by System One" only if the fixture really scored.
grep -q 'POST /v1/systemone' "$tmp/typed.log" || { echo "System One was never called" >&2; exit 1; }

# Tokens only ever lived in $sec; make sure none reached the output or the app.
for f in "$sec/alice" "$sec/bob" "$sec/provider.key"; do
  if grep -rqF -f "$f" -- "$out" "$app" "$stage" 2>/dev/null; then # -f: the token never reaches argv
    echo "a token was found under $out, $app or the staged app; refusing to keep the output" >&2
    exit 1
  fi
done
echo "== wrote:"; ls -l "$out"/semantic-search.png "$out"/scheduled-tasks.png
echo "note: embeddings and System One scores come from search_fixture.py (word overlap, not a semantic model) and the agent's reply from scripted_model.py; the server, index, rerank calls and schedules are real."
exit 0
