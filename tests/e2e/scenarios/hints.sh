# Shortcut helper (src/hints/): the see-through panel in the bottom-right corner, its context,
# the Alt+/ toggle and the saved setting. tmux has no kitty keyboard protocol, so the panel must
# show the fallbacks (Ctrl+P, not Ctrl+Shift+N).

# long_file <lines>: a.txt with long lines, so there is code under the panel.
long_file() {
    local i
    for ((i = 1; i <= $1; i++)); do
        printf 'let value_%d = compute_something_long(alpha, beta, gamma, delta, epsilon);\n' "$i"
    done > a.txt
}

# where <text>: "row col" (0-based, in characters) of the first screen line containing <text>.
where() {
    screen | perl -CS -Mutf8 -ne 'BEGIN { $w = shift; utf8::decode($w) } chomp; my $c = index($_, $w); if ($c >= 0) { print $. - 1, " $c\n"; exit }' "$1"
}

# panel_at <text> <min row> <max row> <min col>: <text> is on screen inside those bounds.
panel_at() {
    wait_for "$1"
    local pos row col
    pos="$(where "$1")"; row="${pos% *}"; col="${pos#* }"
    [ "$row" -ge "$2" ] && [ "$row" -le "$3" ] && [ "$col" -ge "$4" ] || fail "\"$1\" at row $row col $col, want rows $2-$3, col >= $4"
}

t_hints_bottom_right_and_context() {
    E2E_HINTS=1
    long_file 40
    start 100 30 a.txt
    panel_at "editor · IDEA" 18 22 60          # title row of a 9-row panel above the status bar (row 29)
    panel_at "Alt+/" 18 22 90
    panel_at "go to file" 19 28 60
    wait_for "Ctrl+P"
    screen | grep -qF "Ctrl+Shift+N" && fail "no kitty protocol in tmux: the panel must show Ctrl+P"
    screen | grep -qF "compute_something_long" || fail "the code is gone"
    key M-F12                                     # terminal: the panel moves above the dock
    panel_at "terminal" 0 18 60
    wait_for "Ctrl+Alt+↑"
    wait_for "back to editor"
    key C-M-Up
    panel_at "editor · IDEA" 0 18 60
    key M-1                                       # project tree
    panel_at "project tree" 0 18 60
    wait_for "unfold / fold"
    key Escape
    wait_for "editor · IDEA"
    key S-Right                                   # a selection
    wait_for "editor · selection"
    wait_for "duplicate"
}

t_hints_moves_off_the_cursor_line() {
    E2E_HINTS=1
    long_file 40
    start 100 20 a.txt
    panel_at "editor · IDEA" 9 12 60              # bottom right (rows 10..18)
    key C-End                                     # cursor on the last screen line, under the panel
    status "Ln 41"
    panel_at "editor · IDEA" 1 1 60               # now top right, under the tab bar
    key C-Home
    status "Ln 1, Col 1 "
    panel_at "editor · IDEA" 9 12 60              # cursor line at the top: back to the bottom
}

t_hints_toggle_is_saved() {
    E2E_HINTS=1
    long_file 10
    start 100 30 a.txt
    wait_for "editor · IDEA"
    [ -f "$CFG/settings.toml" ] && fail "nothing should be written before a toggle"
    key M-/
    wait_gone "editor · IDEA"
    status "Shortcut hints off"
    grep -qx "hints = false" "$CFG/settings.toml" || fail "not saved: $(cat "$CFG/settings.toml" 2>&1)"
    key C-q; assert_exited
    start 100 30 a.txt                            # restart: stays off
    sleep 0.3
    screen | grep -qF "editor · IDEA" && fail "the helper came back after a restart"
    key M-/
    wait_for "editor · IDEA"
    grep -qx "hints = true" "$CFG/settings.toml" || fail "not saved: $(cat "$CFG/settings.toml")"
}

t_hints_no_hints_flag() {
    E2E_HINTS=1
    long_file 10
    start 100 30 --no-hints a.txt
    wait_for "let value_1 "
    sleep 0.3
    screen | grep -qF "editor · IDEA" && fail "--no-hints still shows the helper"
    [ -f "$CFG/settings.toml" ] && fail "--no-hints must not write the settings"
    key M-/                                       # still toggles on for this run
    wait_for "editor · IDEA"
}
