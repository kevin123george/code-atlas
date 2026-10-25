# M5: terminal pane (Alt+F12), git gutter, Commit panel (Alt+0 / Ctrl+K), diff view.
# Git scenarios make their own repo in $SCRATCH: never a real one.

scratch_repo() {
    git init -q -b main . && git config user.name E2E && git config user.email e2e@example.com \
        && git config commit.gpgsign false || fail "git init failed"
    printf 'bin/\n.expected\n.reason\n.notes\n' >> .git/info/exclude   # the harness's own files
    printf 'one\ntwo\nthree\nfour\nfive\n' > a.txt
    git add a.txt && git commit -qm first || fail "first commit failed"
}

t_terminal_echo() {
    printf 'x\n' > a.txt
    start 100 30 a.txt
    key M-F12
    wait_for "keys go to the shell"
    sleep 1.5                                   # login shells drop keys typed while they start
    typ "echo hi-from-\$((40+2))"; key Enter
    wait_for "hi-from-42" 8
    typ "ls"; key Enter
    wait_for "a.txt" 5
    key C-M-Up                                  # back to the editor, dock stays
    typ "Z"
    wait_line 1 "Zx"
    key M-F12; key M-F12                        # focus the terminal, then hide it
    wait_gone "Ctrl+Alt+↑ editor"
}

t_git_gutter_and_branch() {
    scratch_repo
    start 100 20 a.txt
    status "⎇ main"
    key Down End; typ "!"                       # line 2 changed
    key C-End; typ "six"                        # line 6 added
    wait_for "  2 ▎"
    wait_for "  6 ▎"
}

t_git_commit_panel() {
    scratch_repo
    printf 'new\n' > untracked.txt
    start 100 24 a.txt
    key Down End; typ " changed"; save
    key M-0
    wait_for "Changes 2 · 1 ticked"
    wait_for "[x] M a.txt"
    wait_for "[ ] ? untracked.txt"
    key Enter                                   # diff view
    wait_for "HEAD → working tree"
    wait_for "@@ -1,"
    key Escape
    wait_gone "HEAD → working tree"
    key Tab; typ "E2E commit"
    key M-Enter                                 # Ctrl+Enter's fallback (tmux can't send it)
    status "Committed 1 file(s)"
    [ "$(git log -1 --format=%s)" = "E2E commit" ] || fail "commit not in git log: $(git log --oneline | head -3)"
    [ "$(git status --porcelain)" = "?? untracked.txt" ] || fail "unexpected status: $(git status --porcelain)"
    wait_for "Changes 1 · 0 ticked"
}
