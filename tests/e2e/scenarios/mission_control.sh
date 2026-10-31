# Mission control (decision 0015): two fake agents in their own worktrees at once, land one
# (fast-forward + worktree cleanup), discard the other, presence markers while an agent edits in
# place, asking an agent from a selection. Scratch repos only (scratch_repo is in git_term.sh);
# a fake `claude` and `ssh` in $SCRATCH/bin, never a real run. Worktrees land in "$SCRATCH-wt",
# removed when the scenario ends.

# The fake: --help lists permission modes; a turn logs its args + prompt to bin/, then
#  - "louder": announces an Edit of a.txt, waits 3 s (presence), edits line 3, finishes;
#  - anything else: waits 2 s (so two agents overlap), writes agent-<second word>.txt.
mission_fake() {
    cat > bin/claude <<'EOF'
#!/bin/sh
if [ "$1" = "--help" ]; then echo '  --permission-mode <mode>  (choices: "acceptEdits", "bypassPermissions", "default", "plan")'; exit 0; fi
log="$(dirname "$0")"; n=$(basename "$PWD")
echo "$@" > "$log/args-$n"; cat > "$log/prompt-$n"
sid="e2e-$n"
echo "{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"$sid\",\"model\":\"fake\",\"permissionMode\":\"acceptEdits\"}"
if grep -q louder "$log/prompt-$n"; then
    echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Edit\",\"input\":{\"file_path\":\"$PWD/a.txt\"}}]}}"
    sleep 3
    perl -pi -e 's/^three$/THREE/' a.txt
else
    word=$(head -n 1 "$log/prompt-$n" | awk '{print $2}')
    echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Write\",\"input\":{\"file_path\":\"$PWD/agent-$word.txt\"}}]}}"
    sleep 2
    echo "by the agent" > "agent-$word.txt"
fi
echo "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"ok\"}]}}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":2000,\"num_turns\":1,\"result\":\"ok\",\"session_id\":\"$sid\",\"total_cost_usd\":0.03,\"usage\":{\"input_tokens\":2000,\"output_tokens\":500},\"permission_denials\":[]}"
EOF
    chmod +x bin/claude
    printf '#!/bin/sh\nexit 255\n' > bin/ssh          # the agents panel's staff check: never from tests
    chmod +x bin/ssh
}

# wait_count <n> <string>: until <string> shows on at least <n> screen lines.
wait_count() {
    local n="$1" want="$2" end
    end=$(( $(now_ms) + ${3:-$TIMEOUT} * 1000 ))
    while :; do
        [ "$(screen 2>/dev/null | grep -cF -- "$want")" -ge "$n" ] && return 0
        [ "$(now_ms)" -ge "$end" ] && fail "fewer than $n lines with: $want"
        sleep 0.05
    done
}

t_mission_two_agents_land_discard() {
    scratch_repo; mission_fake
    trap 'rm -rf "$SCRATCH-wt"' EXIT
    start 120 32 a.txt
    tm pipe-pane -t "$SESSION" -O "cat >> '$SCRATCH/bin/out.raw'"
    key C-o
    wait_for "MISSION CONTROL"; wait_for "No agents yet"
    key n; typ "add alpha"; key Enter
    key n; typ "add beta"; key Enter
    wait_for "2 working" 6                              # both at once, each in its own worktree
    wait_for "⎇ add-alpha ← main" 6; wait_for "⎇ add-beta ← main"
    wait_count 2 "waiting for you" 8
    wait_for "1 file +1 −0"
    [ -f "$SCRATCH-wt/add-alpha/agent-alpha.txt" ] || fail "alpha didn't write in its worktree"
    [ -f "$SCRATCH-wt/add-beta/agent-beta.txt" ] || fail "beta didn't write in its worktree"
    [ ! -e agent-alpha.txt ] || fail "the checkout was touched"
    grep -q -- "--permission-mode acceptEdits" bin/args-add-alpha || fail "worktree agents get acceptEdits: $(cat bin/args-add-alpha)"
    grep -q "$(printf '\a')" bin/out.raw || fail "no bell when the agents finished"
    note "two agents in parallel worktrees; bell on finish"
    key Home; key Down                                  # #1 (newest first)
    key l
    wait_for "Land: commit 1 uncommitted file on add-alpha, then fast-forward main"
    key y
    wait_for "Remove its worktree"
    key y
    wait_for "Removed worktree"
    assert_file agent-alpha.txt $'by the agent\n'
    git log -1 --format=%B | grep -q "Co-Authored-By: Claude" || fail "landed commit lacks the trailer"
    [ ! -d "$SCRATCH-wt/add-alpha" ] || fail "worktree still there"
    git rev-parse -q --verify refs/heads/add-alpha >/dev/null && fail "merged branch still there"
    wait_for "landed in main"
    key Up; key x
    wait_for "Can't be undone"
    key y
    wait_for "Discarded"
    [ ! -d "$SCRATCH-wt/add-beta" ] || fail "discarded worktree still there"
    git rev-parse -q --verify refs/heads/add-beta >/dev/null && fail "discarded branch still there"
    [ ! -e agent-beta.txt ] || fail "discarded work reached the checkout"
    git status --porcelain | grep -q . && fail "checkout not clean: $(git status --porcelain)"
    return 0
}

t_mission_presence_markers() {
    scratch_repo; mission_fake
    trap 'rm -rf "$SCRATCH-wt"' EXIT
    start 120 30 . a.txt
    key C-o; wait_for "MISSION CONTROL"
    key n; key Tab; typ "make it louder"; key Enter      # in place
    wait_for "#1"
    key Escape
    wait_gone "MISSION CONTROL"
    wait_for "✳ 1 agent · #1 editing a.txt" 4           # status-bar ticker
    screen | head -1 | grep -qF "a.txt ✳" || fail "no ✳ on the tab"
    screen | grep -E "^ +a\.txt +✳" >/dev/null || fail "no ✳ in the tree"
    wait_for "3 ▎THREE" 8                                # reloaded after the agent's edit (+ git mark)
    wait_gone "✳ 1 agent"
    # Line 3's number in the agent's colour (#1 = S_ORANGE ff9758).
    local end=$(( $(now_ms) + 3000 ))
    until tm capture-pane -p -e -t "$SESSION" | grep "THREE" | grep -q "38;2;255;151;88m"; do
        [ "$(now_ms)" -ge "$end" ] && fail "line 3's number isn't in the agent's colour"
        sleep 0.05
    done
    tm capture-pane -p -e -t "$SESSION" | grep "two" | grep -q "38;2;255;151;88m" && fail "an unchanged line is marked"
    return 0
}

t_mission_ask_from_selection() {
    scratch_repo; mission_fake
    trap 'rm -rf "$SCRATCH-wt"' EXIT
    start 120 30 a.txt
    key Down S-Down S-Down                              # lines 2-3
    key C-u
    wait_for "Ask an agent · a.txt:2-3"
    wait_for "write tests"
    key Enter                                            # explain: in place, answer in the panel
    wait_for "explain a.txt:2-3"
    wait_for "Agents"
    local p="bin/prompt-$(basename "$SCRATCH")"
    local end=$(( $(now_ms) + 4000 ))
    until [ -s "$p" ]; do [ "$(now_ms)" -ge "$end" ] && fail "no prompt"; sleep 0.05; done
    grep -q "^Explain this code" "$p" || fail "prompt: $(head -1 "$p")"
    grep -q "a.txt (lines 2-3 selected)" "$p" || fail "no file/lines in the prompt"
    grep -q "    3  three" "$p" || fail "no selected text in the prompt"
    # (the selection stays for the next ask)
    key C-u; wait_for "Ask an agent"
    key Down; key Enter                                  # fix: in a new worktree
    wait_for "started in" 6
    key C-o
    wait_for "⎇ fix-a-txt-2-3 ← main" 4
    wait_for "#2"
    return 0
}
