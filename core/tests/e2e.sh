#!/usr/bin/env zsh
# End-to-end test of lodestone_core: server, owner setup, a real Minecraft
# instance, macros (glue + lodestone-macro-lib), teardown.
# The server and the client must run in the same command: each sandboxed
# command has its own network namespace.
#
# Needs network access (Mojang, Adoptium, GitHub, deno.land), plus curl, jq, rg.
# Uses a throwaway --lodestone-path, so it does not touch ~/.lodestone.
#
# Usage: core/tests/e2e.sh [binary] [workdir]
#   E2E_MC_VERSION (default 1.21.1), E2E_MC_PORT (default 25599)
emulate -L zsh
setopt no_unset pipe_fail

REPO=${0:A:h:h:h}
BIN=${1:-$REPO/target/debug/lodestone_core}
WORK=${2:-${TMPDIR:-/tmp}/lodestone-e2e}
VERSION=${E2E_MC_VERSION:-1.21.1}
MC_PORT=${E2E_MC_PORT:-25599}

rm -rf "$WORK/home"
mkdir -p "$WORK/home"
LOG=$WORK/server.log
PASS=0
FAIL=0

ok()   { PASS=$((PASS+1)); printf 'PASS  %s\n' "$*"; }
bad()  { FAIL=$((FAIL+1)); printf 'FAIL  %s\n' "$*"; }
log()  { printf '----  %s\n' "$*"; }
plain_log() { sed 's/\x1b\[[0-9;]*m//g' "$LOG"; }

# wait_for <seconds> <description> <command...>
wait_for() {
  local secs=$1 what=$2; shift 2
  local i
  for i in {1..$secs}; do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 1
  done
  printf 'timeout after %ss waiting for %s\n' "$secs" "$what"
  return 1
}

RUST_LOG=${RUST_LOG:-info} "$BIN" --is-cli --lodestone-path "$WORK/home" >"$LOG" 2>&1 &
SERVER_PID=$!
cleanup() { kill $SERVER_PID 2>/dev/null; wait $SERVER_PID 2>/dev/null; }
trap cleanup EXIT INT TERM

wait_for 60 "server to listen" rg -q "live on" "$LOG" || { plain_log | tail -20; return 1; }
# A debug build moves to the next free port if 16662 is taken, so read the
# port from the log rather than risk talking to another Lodestone.
PORT=$(plain_log | rg -o 'live on \[::\]:(\d+)' -r '$1')
[[ -n $PORT ]] || { printf 'could not find the port in %s\n' "$LOG"; return 1; }
B=http://localhost:$PORT/api/v1
ok "server started on port $PORT"

KEY=$(plain_log | rg -o 'setup key: (\S+)' -r '$1')
T=$(curl -sf -X POST "$B/setup/$KEY" -H 'Content-Type: application/json' \
      -d '{"username":"owner","password":"hunter2hunter2"}' | jq -r .token)
[[ -n $T && $T != null ]] && ok "owner setup" || { bad "owner setup"; return 1; }
AUTH=(-H "Authorization: Bearer $T")
api() { local m=$1 p=$2; shift 2; curl -sS -X "$m" "${AUTH[@]}" -H 'Content-Type: application/json' "$B$p" "$@"; }

T2=$(curl -sf -X POST -u owner:hunter2hunter2 "$B/user/login" | jq -r .token)
[[ -n $T2 && $T2 != null ]] && ok "login" || bad "login"

# --- create a vanilla instance --------------------------------------------
setup_value=$(jq -n --arg v "$VERSION" --argjson port $MC_PORT '{
  name: "e2e", description: "e2e test", auto_start: false, restart_on_crash: false,
  setting_sections: {
    section_1: { settings: {
      version: { value: { type: "Enum", value: $v } },
      port:    { value: { type: "UnsignedInteger", value: $port } } } },
    section_2: { settings: {
      min_ram:  { value: { type: "UnsignedInteger", value: 1024 } },
      max_ram:  { value: { type: "UnsignedInteger", value: 2048 } },
      cmd_args: { value: null } } } } }')
resp=$(api POST /instance/create/MinecraftJavaVanilla -d "$setup_value")
UUID=$(jq -r . <<<"$resp" 2>/dev/null)
[[ $UUID == INSTANCE_* ]] && ok "create instance request ($UUID)" || { bad "create instance: $resp"; return 1; }

have_instance() { api GET /instance/list | jq -e --arg u "$UUID" 'map(select(.uuid == $u)) | length == 1'; }
log "waiting for instance setup (downloads server jar and JRE)"
wait_for 600 "instance setup" have_instance && ok "instance set up" || { bad "instance set up"; plain_log | rg -i "error|warn" | tail -20; return 1; }
IDIR=$(print -r -- $WORK/home/instances/e2e-*(N/[1]))
log "instance dir: $IDIR"

state() { api GET /instance/$UUID/info | jq -r .state; }
console() { api GET /instance/$UUID/console/buffer; }

# --- start the server -----------------------------------------------------
api PUT /instance/$UUID/start >/dev/null
is_running() { [[ $(state) == Running ]]; }
wait_for 300 "instance Running" is_running && ok "instance Running" || { bad "instance Running (state=$(state))"; console | jq -r '.[-20:][] | .event_inner | tostring' 2>/dev/null | tail -20; }

api POST /instance/$UUID/console -d '"say hello from the api"' >/dev/null
console_has() { console | rg -qF -- "$1"; }
wait_for 20 "console echo" console_has "hello from the api" && ok "console command" || bad "console command"

# --- a macro through the embedded glue and lodestone-macro-lib -------------
mkdir -p "$IDIR/macros"
cat >"$IDIR/macros/e2e.ts" <<'EOF'
import { MinecraftJavaInstance } from "https://raw.githubusercontent.com/Lodestone-Team/lodestone-macro-lib/main/instance.ts";
import { EventStream } from "https://raw.githubusercontent.com/Lodestone-Team/lodestone-macro-lib/main/events.ts";

const inst = await MinecraftJavaInstance.current();
const es = new EventStream(inst.getUUID(), await inst.name());
let denied = "no";
try { await Deno.readTextFile("/etc/hostname"); } catch (e) {
  denied = e instanceof Deno.errors.PermissionDenied ? "yes" : String(e);
}
const info = {
  name: await inst.name(), state: await inst.state(), version: await inst.gameVersion(),
  port: await inst.port(), players: await inst.playerCount(), cwd: Deno.cwd(),
  denied, fetch: (await fetch("https://raw.githubusercontent.com/Lodestone-Team/lodestone-macro-lib/main/prelude.ts")).status,
};
await Deno.writeTextFile("e2e-macro-output.json", JSON.stringify(info));
await inst.sendCommand("say hello from the e2e macro");
es.emitConsoleOut("[e2e] macro done");
EOF
resp=$(api PUT /instance/$UUID/macro/run/e2e -d "[]"); [[ $resp == null ]] || bad "run e2e macro: $resp"
wait_for 60 "macro output file" test -f "$IDIR/e2e-macro-output.json" && ok "macro ran and wrote a file" || bad "macro output file"
if [[ -f $IDIR/e2e-macro-output.json ]]; then
  out=$(<"$IDIR/e2e-macro-output.json"); log "macro output: $out"
  jq -e --arg v "$VERSION" --argjson p $MC_PORT --arg d "$IDIR" \
    '.name=="e2e" and .state=="Running" and .version==$v and .port==$p and .players==0 and .denied=="yes" and .fetch==200 and ((.cwd|rtrimstr("/")) == ($d|rtrimstr("/")))' \
    <<<"$out" >/dev/null && ok "macro saw the right instance, fs sandbox, fetch" || bad "macro output values"
fi
wait_for 20 "macro console command" console_has "hello from the e2e macro" && ok "macro sendCommand" || bad "macro sendCommand"
wait_for 20 "macro event" console_has "[e2e] macro done" && ok "macro emitConsoleOut" || bad "macro emitConsoleOut"
macro_exited() { api GET /instance/$UUID/history/list | jq -e 'map(select(.task.name=="e2e")) | length >= 1'; }
wait_for 30 "macro in history" macro_exited && ok "macro in history: $(api GET /instance/$UUID/history/list | jq -c 'map(select(.task.name=="e2e"))[0] | .exit_status')" || bad "macro not in history: $(api GET /instance/$UUID/history/list)"

# --- the auto-backup example ----------------------------------------------
# Same flow as the dashboard: a macro with a LodestoneConfig class needs its
# config fetched and stored (macros/<name>/<name>_config.json) before it runs.
cp -r "$REPO/example-macros/auto-backup" "$IDIR/macros/"
cfg=$(api GET /instance/$UUID/macro/config/get/auto-backup)
log "auto-backup config: $(jq -c '.config | map_values(.value // .default_value)' <<<"$cfg")"
jq -e ".config.delaySec.value.value == 3600 and (.error == null or .error == \"NotFound\")" <<<"$cfg" >/dev/null && ok "macro config parsed" || bad "macro config: $cfg"
code=$(api POST /instance/$UUID/macro/config/store/auto-backup -o /dev/null -w "%{http_code}" \
         -d "$(jq '.config | map_values(.value = (.value // .default_value))' <<<"$cfg")")
[[ $code == 200 ]] && [[ -f $IDIR/macros/auto-backup/auto-backup_config.json ]] && ok "macro config stored" || bad "macro config store: HTTP $code"
resp=$(api PUT /instance/$UUID/macro/run/auto-backup -d '[]')
[[ $resp == null ]] || bad "run auto-backup: $resp"
has_backup() { [[ -n $(print -r -- $IDIR/backups/backup_*(N/)) ]] && [[ -f $(print -r -- $IDIR/backups/backup_*(N/[1]))/level.dat ]]; }
wait_for 90 "backup folder" has_backup && ok "auto-backup copied the world" || bad "auto-backup (backups: $(ls "$IDIR/backups" 2>&1))"
PID_AB=$(api GET /instance/$UUID/task/list | jq -r 'map(select(.name=="auto-backup"))[0].pid // empty')
if [[ -n $PID_AB ]]; then
  api PUT /instance/$UUID/macro/kill/$PID_AB >/dev/null
  not_running_ab() { api GET /instance/$UUID/task/list | jq -e 'map(select(.name=="auto-backup")) | length == 0'; }
  wait_for 20 "auto-backup killed" not_running_ab && ok "kill macro" || bad "kill macro"
else
  bad "auto-backup not in task list: $(api GET /instance/$UUID/task/list)"
fi

# --- stop and delete --------------------------------------------------------
api PUT /instance/$UUID/stop >/dev/null
is_stopped() { [[ $(state) == Stopped ]]; }
wait_for 60 "instance Stopped" is_stopped && ok "instance Stopped" || bad "instance Stopped (state=$(state))"
api DELETE /instance/$UUID >/dev/null
gone() { api GET /instance/list | jq -e --arg u "$UUID" 'map(select(.uuid == $u)) | length == 0'; }
wait_for 30 "instance deleted" gone && ok "instance deleted" || bad "instance deleted"

kill -0 $SERVER_PID 2>/dev/null && ok "server still alive" || bad "server died"
log "server log errors:"
plain_log | rg "ERROR|panicked" | rg -v docker_bridge | tail -20
printf '\n%d passed, %d failed\n' $PASS $FAIL
(( FAIL == 0 ))
