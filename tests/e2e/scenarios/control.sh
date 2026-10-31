# Control Center (Alt+O) and Find Action (Ctrl+O; Ctrl+Shift+A needs the kitty protocol, which
# tmux lacks), decision 0016. A fake `ssh` keeps the Aa staff check off the network.

no_ssh() { printf '#!/bin/sh\nexit 255\n' > bin/ssh; chmod +x bin/ssh; }

t_palette_runs_an_action() {
    no_ssh
    printf 'one\ntwo\n' > a.txt
    start 100 30 a.txt
    key C-o
    wait_for "Find Action"
    typ "dupl"
    wait_for "Duplicate line or selection"
    wait_for "Ctrl+D"
    key Enter
    wait_gone "Find Action"
    wait_line 2 "one"
    wait_line 3 "two"
    save
    assert_file a.txt $'one\none\ntwo\n'
    key C-o                                       # recently used first
    wait_for "recent"
    key Escape
    wait_gone "Find Action"
}

t_control_toggle_persists() {
    no_ssh
    printf 'x\n' > a.txt
    start 100 30 a.txt
    printf '# my notes\nhints = false   # keep it quiet\n' > "$CFG/settings.toml"
    key M-o
    wait_for "CONTROL CENTER"
    key 2
    wait_for "Autocomplete"
    key Down Down Down                            # theme, vim, hints → autocomplete
    key Enter
    status "Autocomplete: off · saved"
    grep -qx "autocomplete = false" "$CFG/settings.toml" || fail "not saved: $(cat "$CFG/settings.toml")"
    grep -qx "# my notes" "$CFG/settings.toml" || fail "comment lost: $(cat "$CFG/settings.toml")"
    grep -qx "hints = false   # keep it quiet" "$CFG/settings.toml" || fail "line changed: $(cat "$CFG/settings.toml")"
    key Escape Escape
    wait_gone "CONTROL CENTER"
    key C-q; assert_exited
    start 100 30 a.txt                            # restart: still off
    key M-o; key 2
    wait_for "Autocomplete"
    screen | grep "Autocomplete" | grep -q "off" || fail "autocomplete came back on"
}

# bg_at <row> <col>: the background colour code of a cell (from capture-pane -e), e.g. "48;2;246;247;249".
t_theme_switch_live() {
    no_ssh
    printf 'fn main() {}\n' > a.rs
    start 100 30 a.rs
    tm capture-pane -e -p -t "$SESSION" | grep -q "48;2;9;11;16" || fail "dark paper missing"
    key C-o
    typ "light theme"
    wait_for "Ledger Terminal light"
    key Enter
    wait_gone "Find Action"
    local end=$(( $(now_ms) + 3000 ))
    until tm capture-pane -e -p -t "$SESSION" | grep -q "48;2;246;247;249"; do
        [ "$(now_ms)" -ge "$end" ] && fail "no light paper after the switch"
        sleep 0.05
    done
    tm capture-pane -e -p -t "$SESSION" | grep -q "48;2;9;11;16" && fail "dark paper left on screen"
    grep -qx 'theme = "ledger-light"' "$CFG/settings.toml" || fail "theme not saved: $(cat "$CFG/settings.toml")"
}

t_control_center_80_and_200() {
    no_ssh
    printf 'x\n' > a.txt
    git init -q -b main . && git -c user.name=T -c user.email=t@e add a.txt && git -c user.name=T -c user.email=t@e -c commit.gpgsign=false commit -qm first
    start 80 24 a.txt
    key M-o
    wait_for "CONTROL CENTER"
    wait_for "WORKSPACE"
    wait_for "⎇ main"
    screen | perl -CS -ne 'chomp; exit 1 if length > 80' || fail "wider than 80 columns"
    key Tab Tab                                   # language servers: the view scrolls to it
    wait_for "LANGUAGE SERVERS"
    key Escape
    wait_gone "CONTROL CENTER"
    key C-q; assert_exited
    start 200 50 a.txt
    key M-o
    wait_for "EDITOR"
    local row
    row="$(screen | grep "WORKSPACE")"
    case "$row" in *AGENTS*"LANGUAGE SERVERS"*) ;; *) fail "three tiles in a row: $row" ;; esac
    wait_for "PROCESSES"
    wait_for "catlas"
    wait_for "laptop unreachable (ssh)"
}
