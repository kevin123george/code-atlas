# Git view (decision 0012): live review, stage / revert hunks, partial commit, log graph, an agent
# run's review (fake `claude`), blame. Scratch repos only (scratch_repo is in git_term.sh); the
# remote actions are covered by unit tests against a local bare repo, never a real remote.

# A fake `claude -p`: edits a.txt with its Edit tool and creates made.txt "by shell".
fake_claude() {
    cat > bin/claude <<'EOF'
#!/bin/sh
[ "$1" = "--help" ] && { echo "Usage: claude"; exit 0; }
cat > /dev/null
d="$PWD"; sid="e2e00000-0000-0000-0000-000000000001"
echo "{\"type\":\"system\",\"subtype\":\"init\",\"cwd\":\"$d\",\"session_id\":\"$sid\",\"model\":\"fake\",\"permissionMode\":\"acceptEdits\"}"
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Edit\",\"input\":{\"file_path\":\"$d/a.txt\",\"old_string\":\"three\",\"new_string\":\"THREE\"}}]},\"session_id\":\"$sid\"}"
perl -pi -e 's/^three$/THREE (by the agent)/' a.txt
echo "{\"type\":\"user\",\"message\":{\"content\":[{\"tool_use_id\":\"t1\",\"type\":\"tool_result\",\"content\":\"ok\"}]},\"session_id\":\"$sid\"}"
printf 'made by the agent\n' > made.txt
echo "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Changed a.txt, added made.txt.\"}]},\"session_id\":\"$sid\"}"
echo "{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"duration_ms\":300,\"num_turns\":2,\"result\":\"ok\",\"session_id\":\"$sid\",\"total_cost_usd\":0.01,\"permission_denials\":[]}"
EOF
    chmod +x bin/claude
    # The agents panel checks the Aa staff over ssh: never from tests.
    printf '#!/bin/sh\nexit 255\n' > bin/ssh
    chmod +x bin/ssh
}

t_git_view_live_refresh() {
    scratch_repo
    start 100 24 a.txt
    key M-8                                     # Alt+8 (Ctrl+L is the other key)
    wait_for "No changes: the working tree matches HEAD"
    local t0; t0=$(now_ms)
    printf 'six\n' >> a.txt                     # changed outside the editor (an agent, a shell)
    wait_for "+ six" 4
    note "change shown after $(( $(now_ms) - t0 )) ms"
    printf 'fresh\n' > new.txt
    wait_for "? new.txt" 4
    wait_for "working tree 2 files +2 −0"
    key Escape
    wait_gone "1 Changes"
    wait_line 1 "one"                           # the editor is back
}

t_git_view_stage_revert_partial_commit() {
    scratch_repo
    perl -e 'print "line $_\n" for 1..12' > b.txt
    git add b.txt && git commit -qm twelve
    perl -pi -e 's/^line 1$/LINE ONE/; s/^line 12$/LINE TWELVE/' b.txt
    start 110 26 a.txt
    key C-l
    wait_for "+ LINE ONE"; wait_for "· 1/2"
    key s                                       # stage the first hunk only
    status "Staged a hunk of b.txt"
    wait_for "STAGED · 1 file"
    git diff --cached | grep -q '^+LINE ONE' || fail "first hunk not staged: $(git diff --cached)"
    git diff --cached | grep -q 'TWELVE' && fail "second hunk staged too"
    # Partial commit from the Commit panel: only the staged hunk goes in.
    key M-0
    wait_for "M b.txt · staged hunks only"
    key Tab; typ "only line one"
    key M-Enter
    status "Committed 1 file(s)"
    git show HEAD | grep -q '^+LINE ONE' || fail "commit lacks the staged hunk"
    git show HEAD | grep -q 'TWELVE' && fail "the unstaged hunk was committed"
    grep -q 'LINE TWELVE' b.txt || fail "the working tree lost the unstaged hunk"
    key Escape                                  # the commit panel lets go: the review has the keys again
    wait_for "+ LINE TWELVE"
    wait_gone "STAGED"
    key r
    wait_for "Revert this hunk of b.txt (+1 −1)?"
    key y
    status "Reverted in b.txt"
    grep -q 'TWELVE' b.txt && fail "not reverted: $(tail -2 b.txt)"
    wait_for "No changes: the working tree matches HEAD"
    key u
    status "Undone"
    grep -q '^LINE TWELVE$' b.txt || fail "undo didn't bring the hunk back"
    wait_for "+ LINE TWELVE"
}

t_git_view_log_graph() {
    scratch_repo
    git checkout -qb side && printf 'side\n' > s.txt && git add s.txt && git commit -qm "side work"
    git checkout -q main && printf 'more\n' >> a.txt
    git commit -qam "main work" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
    git merge -q --no-edit side
    start 100 24 a.txt
    key C-l; key 2
    wait_for "●─╮"
    wait_for "[main] Merge branch 'side'"
    wait_for "✳ main work"
    wait_for "[side] side work"
    wait_for "●─╯"
    wait_for "4 of 4 commits"
    key Enter                                   # details of the merge (vs its first parent)
    wait_for "commit $(git rev-parse HEAD)"
    wait_for "A s.txt  +1 −0"
    key Escape
    wait_for "●─╮"
    key /; typ "side"
    wait_for "2 of 4 commits"
    key Enter
    key h                                       # a.txt's history (the file the editor had)
    wait_for "history of a.txt"
    wait_for "main work"
    wait_gone "side work"
}

t_git_view_agent_run_review() {
    scratch_repo
    fake_claude
    start 130 30 a.txt
    key M-9
    wait_for "+ New agent"
    typ "make three louder"; key Enter
    wait_for "since the run started: 2 files +2 −1" 8
    key C-d                                     # review what this run changed
    wait_for "agent run #1"
    wait_for "+ THREE (by the agent)"
    wait_for "A made.txt"
    wait_for "✳ #1 make three louder"
    key t                                       # only the files its edit tools wrote
    wait_for "run #1 1 file +1 −1"
    wait_gone "A made.txt"
    key R
    wait_for "Revert everything since the run started in a.txt"
    key y
    status "Reverted in a.txt"
    assert_file a.txt $'one\ntwo\nthree\nfour\nfive\n'
    [ -f made.txt ] || fail "made.txt should stay (not part of this file's revert)"
    git diff --quiet -- a.txt || fail "a.txt should match HEAD again"
}

t_git_view_blame() {
    scratch_repo
    printf 'one\nTWO\nthree\nfour\nfive\n' > a.txt
    git commit -qam "two in capitals" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
    start 100 20 a.txt
    key C-l; key b
    status "Blame on"
    wait_for "✳now E2E"                         # line 2 came with Claude's commit
    wait_for " now E2E"
    key Down; key C-l                           # that line's commit
    wait_for "two in capitals"
    wait_for "commit $(git rev-parse HEAD)"
}
