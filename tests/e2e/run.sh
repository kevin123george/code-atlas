#!/usr/bin/env bash
# End-to-end tests for Code Atlas: drive the real `catlas` binary in tmux, send keys, read the screen,
# check saved files. No extra dependencies beyond bash, tmux and perl (for millisecond timing).
#
#   tests/e2e/run.sh              # build release, run every scenario
#   tests/e2e/run.sh pair undo    # only scenarios whose name contains "pair" or "undo"
#   tests/e2e/run.sh --list       # list scenarios
#   E2E_NO_BUILD=1 / CATLAS=path  # skip the build / test another binary
#   E2E_KEEP=1                    # keep scratch dirs (paths are printed)
#
# Each scenario runs in a subshell with its own scratch dir under $TMPDIR and its own tmux session on a
# private tmux socket (`-L catlas-e2e-<pid>`), so your own tmux sessions are never touched. On failure
# the screen is saved to tests/e2e/out/<scenario>.txt. Ctrl+C/X call a fake `pbcopy` (first on PATH)
# that writes $SCRATCH/clipboard, so tests never touch the real system clipboard.
set -u

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$E2E_DIR/../.." && pwd)"
OUT="$E2E_DIR/out"
TMUX_BIN="${TMUX_BIN:-$(command -v tmux || echo /opt/homebrew/bin/tmux)}"
SOCK="catlas-e2e-$$"
CATLAS="${CATLAS:-$ROOT/target/release/catlas}"
TIMEOUT="${E2E_TIMEOUT:-3}"     # seconds for wait_* helpers
unset TMUX                       # allow running from inside tmux

# ---- helpers ------------------------------------------------------------------------------------

tm() { "$TMUX_BIN" -L "$SOCK" "$@"; }

now_ms() {
    if [ -n "${EPOCHREALTIME:-}" ]; then
        local t="${EPOCHREALTIME/[.,]/}"; echo $(( t / 1000 ))
    else
        perl -MTime::HiRes=time -e 'printf "%d\n", time*1000'
    fi
}

# fail <reason>: save the screen (and the reason) to out/<scenario>.txt and end the scenario.
fail() {
    mkdir -p "$OUT"
    {
        echo "scenario: $SCEN"
        echo "reason:   $*"
        echo "---- screen ----"
        screen 2>/dev/null || echo "(no session: the editor exited)"
        echo "---- cursor (x,y): $(tm display -p -t "$SESSION" '#{cursor_x},#{cursor_y}' 2>/dev/null) ----"
    } > "$OUT/$SCEN.txt"
    echo "$*" > "$SCRATCH/.reason"
    exit 1
}

# start <cols> <rows> [file ...]: run catlas in a new tmux session (cwd = scratch dir), wait for the UI.
start() {
    local cols="$1" rows="$2"; shift 2
    local cmd="cd '$SCRATCH' && PATH='$SCRATCH/bin:$PATH' exec '$CATLAS'"
    local f; for f in "$@"; do cmd="$cmd '$f'"; done
    tm new-session -d -s "$SESSION" -x "$cols" -y "$rows" "$cmd" || fail "tmux new-session failed"
    tm set -t "$SESSION" status off >/dev/null 2>&1
    wait_for "Code Atlas"
    # Let the first full draw (and the keyboard-protocol query) finish.
    wait_for " IDEA"
}

screen() { tm capture-pane -p -t "$SESSION"; }

# typ <text>: literal text, no key names. tmux reads a trailing ";" as a command separator,
# so that one goes as "\;".
typ() {
    local s="$1" n=0
    while [ -n "$s" ] && [ "${s%;}" != "$s" ]; do s="${s%;}"; n=$((n + 1)); done
    [ -n "$s" ] && tm send-keys -t "$SESSION" -l -- "$s"
    while [ $n -gt 0 ]; do tm send-keys -t "$SESSION" -l "\;"; n=$((n - 1)); done
    return 0
}

# key <name> [...]: tmux key names (Enter, BSpace, Tab, BTab, Escape, Home, End, PPage, NPage,
# Left/Right/Up/Down, F3, S-F3, C-a, M-z, M-S-Up, C-End ...). A lone Escape gets a pause so it
# isn't read as Alt+<next key>.
key() {
    local k
    for k in "$@"; do
        tm send-keys -t "$SESSION" "$k"
        case "$k" in Escape) sleep 0.15 ;; esac
    done
}

# raw <hex bytes...>: send exact bytes, e.g. what a kitty-protocol terminal sends for keys tmux
# can't produce: raw 1b 5b 31 32 32 3b 36 75  (ESC[122;6u = Ctrl+Shift+Z).
raw() { tm send-keys -t "$SESSION" -H "$@"; }

# repeat <n> <key>
repeat() { local i; for ((i = 0; i < $1; i++)); do key "$2"; done; }

# wait_for <string> [timeout]: until the screen contains <string> (fixed string).
wait_for() {
    local want="$1" t="${2:-$TIMEOUT}" end
    end=$(( $(now_ms) + t * 1000 ))
    while :; do
        screen 2>/dev/null | grep -qF -- "$want" && return 0
        [ "$(now_ms)" -ge "$end" ] && fail "timed out waiting for: $want"
        sleep 0.03
    done
}

# wait_gone <string>: until the screen no longer contains it.
wait_gone() {
    local want="$1" t="${2:-$TIMEOUT}" end
    end=$(( $(now_ms) + t * 1000 ))
    while :; do
        screen 2>/dev/null | grep -qF -- "$want" || return 0
        [ "$(now_ms)" -ge "$end" ] && fail "still on screen after ${t}s: $want"
        sleep 0.03
    done
}

# status <text>: wait until the status bar (last row) contains <text>, e.g. status "Ln 3, Col 2 ".
status() {
    local want="$1" end
    end=$(( $(now_ms) + TIMEOUT * 1000 ))
    while :; do
        screen 2>/dev/null | tail -n 1 | grep -qF -- "$want" && return 0
        [ "$(now_ms)" -ge "$end" ] && fail "status bar never showed: $want"
        sleep 0.03
    done
}

# text_line <n>: what the editor shows for buffer line n (gutter stripped, trailing spaces trimmed).
text_line() {
    screen | awk -v n="$1" '
        NR > 1 { if (match($0, "^ *" n "  ")) { print substr($0, RLENGTH + 1); exit } }'
}

# wait_line <n> <text>: until buffer line n shows exactly <text>.
wait_line() {
    local n="$1" want="$2" end got
    end=$(( $(now_ms) + TIMEOUT * 1000 ))
    while :; do
        got="$(text_line "$n" 2>/dev/null)"
        [ "$got" = "$want" ] && return 0
        [ "$(now_ms)" -ge "$end" ] && fail "line $n: want [$want], got [$got]"
        sleep 0.03
    done
}

# cursor_at <x> <y>: until the terminal cursor is at screen cell (x, y), both 0-based.
cursor_at() {
    local want="$1,$2" end got
    end=$(( $(now_ms) + TIMEOUT * 1000 ))
    while :; do
        got="$(tm display -p -t "$SESSION" '#{cursor_x},#{cursor_y}')"
        [ "$got" = "$want" ] && return 0
        [ "$(now_ms)" -ge "$end" ] && fail "cursor: want ($want), got ($got)"
        sleep 0.03
    done
}

# save: Ctrl+S and wait for the "Saved" message.
save() { key C-s; status "Saved"; }

# assert_file <path> <expected>: exact bytes (use $'...' for \n, \r, \t).
assert_file() {
    local path="$1" want="$2"
    [ -f "$path" ] || fail "file missing: $path"
    printf '%s' "$want" > "$SCRATCH/.expected"
    if ! cmp -s "$SCRATCH/.expected" "$path"; then
        fail "file $path: want $(od -c < "$SCRATCH/.expected" | head -5 | tr -s ' ' | tr '\n' '|') got $(od -c < "$path" | head -5 | tr -s ' ' | tr '\n' '|')"
    fi
}

# assert_clipboard <expected>: what the editor sent to the (fake) system clipboard.
assert_clipboard() {
    local end=$(( $(now_ms) + TIMEOUT * 1000 ))
    printf '%s' "$1" > "$SCRATCH/.expected"
    until cmp -s "$SCRATCH/.expected" "$SCRATCH/clipboard" 2>/dev/null; do
        [ "$(now_ms)" -ge "$end" ] && fail "clipboard: want [$1], got [$(cat "$SCRATCH/clipboard" 2>/dev/null)]"
        sleep 0.03
    done
}

# alive / assert_exited: whether catlas (the session) is still running.
alive() { tm has-session -t "$SESSION" 2>/dev/null; }
assert_exited() {
    local end=$(( $(now_ms) + TIMEOUT * 1000 ))
    while alive; do
        [ "$(now_ms)" -ge "$end" ] && fail "editor did not quit"
        sleep 0.03
    done
}

# note <text>: an extra line for the results table (timings, limitations).
note() { echo "$*" >> "$SCRATCH/.notes"; }

# ---- runner -------------------------------------------------------------------------------------

for f in "$E2E_DIR"/scenarios/*.sh; do . "$f"; done
ALL=$(declare -F | awk '{print $3}' | grep '^t_' | sed 's/^t_//')

if [ "${1:-}" = "--list" ]; then echo "$ALL"; exit 0; fi

selected=""
for s in $ALL; do
    if [ $# -eq 0 ]; then selected="$selected $s"; continue; fi
    for pat in "$@"; do case "$s" in *"$pat"*) selected="$selected $s"; break ;; esac; done
done
[ -n "$selected" ] || { echo "no scenario matches: $*"; exit 2; }

if [ -z "${E2E_NO_BUILD:-}" ] && [ "$CATLAS" = "$ROOT/target/release/catlas" ]; then
    (cd "$ROOT" && PATH="$HOME/.cargo/bin:$PATH" cargo build --release --quiet) || { echo "build failed"; exit 2; }
fi
[ -x "$CATLAS" ] || { echo "no binary at $CATLAS"; exit 2; }
[ -x "$TMUX_BIN" ] || { echo "tmux not found (set TMUX_BIN)"; exit 2; }

cleanup_server() {
    tm kill-server >/dev/null 2>&1
    rm -f -- "${TMUX_TMPDIR:-/tmp}/tmux-$(id -u)/$SOCK"   # kill-server leaves the socket file
}
trap cleanup_server EXIT INT TERM
mkdir -p "$OUT"

pass=0; failed=0; failed_names=""
printf '%-26s %-5s %7s  %s\n' SCENARIO RESULT MS DETAIL
for s in $selected; do
    SCEN="$s"
    SESSION="e2e-$s"
    SCRATCH="$(mktemp -d "${TMPDIR:-/tmp}/catlas-e2e-$s.XXXXXX")"
    mkdir -p "$SCRATCH/bin"
    printf '#!/bin/sh\ncat > "%s/clipboard"\n' "$SCRATCH" > "$SCRATCH/bin/pbcopy"
    chmod +x "$SCRATCH/bin/pbcopy"
    rm -f "$OUT/$s.txt"
    t0=$(now_ms)
    ( cd "$SCRATCH" && "t_$s" )
    rc=$?
    ms=$(( $(now_ms) - t0 ))
    tm kill-session -t "$SESSION" >/dev/null 2>&1
    detail=""
    [ -f "$SCRATCH/.notes" ] && detail="$(tr '\n' ';' < "$SCRATCH/.notes" | sed 's/;$//')"
    if [ $rc -eq 0 ]; then
        pass=$((pass + 1)); printf '%-26s %-5s %7s  %s\n' "$s" PASS "$ms" "$detail"
    else
        failed=$((failed + 1)); failed_names="$failed_names $s"
        reason="$(cat "$SCRATCH/.reason" 2>/dev/null || echo "exit $rc")"
        printf '%-26s %-5s %7s  %s\n' "$s" FAIL "$ms" "$reason${detail:+ ($detail)}"
    fi
    if [ -n "${E2E_KEEP:-}" ]; then
        echo "   scratch kept: $SCRATCH"
    else
        case "$SCRATCH" in */catlas-e2e-*) rm -rf -- "$SCRATCH" ;; esac
    fi
done

echo
echo "$pass passed, $failed failed${failed_names:+ ->$failed_names} (screens in tests/e2e/out/)"
[ $failed -eq 0 ]
