# New file + Save as, tabs, close and quit confirmations, line endings.

t_new_save_as() {
    printf 'a\n' > a.txt
    start 100 12 a.txt
    key C-n
    wait_for "▎untitled"
    typ "new text"
    key C-s
    wait_for "Save as: "
    typ "sub/new.txt"; key Enter
    status "Saved new.txt"
    wait_for "▎new.txt"
    assert_file sub/new.txt "new text"
    assert_file a.txt $'a\n'
    key M-Left
    wait_for "▎a.txt"
}

t_save_as_cancel() {
    printf "a\n" > a.txt
    start 100 12 a.txt                       # (no file at all = project tree has focus since M2)
    key C-n
    wait_for "▎untitled"
    typ "draft"
    key C-s; wait_for "Save as: "; key Escape
    wait_gone "Save as: "
    wait_line 1 "draft"
    wait_for "untitled ●"
}

t_tabs_switch() {
    printf 'A\n' > a.txt; printf 'B\n' > b.txt; printf 'C\n' > c.txt
    start 100 12 a.txt b.txt c.txt
    wait_for "▎a.txt"
    key M-Right; wait_for "▎b.txt"; wait_line 1 "B"
    key M-Right; wait_for "▎c.txt"; wait_line 1 "C"
    key M-Right; wait_for "▎a.txt"           # wraps
    key M-Left;  wait_for "▎c.txt"
    typ "x"; save
    assert_file c.txt $'xC\n'
    assert_file a.txt $'A\n'
    assert_file b.txt $'B\n'
}

t_close_tab_confirm() {
    printf 'A\n' > a.txt; printf 'B\n' > b.txt
    start 100 12 a.txt b.txt
    typ "zz"
    key C-w
    wait_for "Not saved. Ctrl+W again closes without saving."
    wait_for "a.txt ●"
    typ "q"                                  # any other key disarms
    key C-w
    wait_for "Not saved. Ctrl+W again"
    key C-w                                  # second press closes
    wait_gone "a.txt"
    wait_for "▎b.txt"
    assert_file a.txt $'A\n'
    key C-w                                  # clean tab closes at once; last tab -> untitled
    wait_for "▎untitled"
}

t_quit_confirm() {
    printf 'A\n' > a.txt
    start 100 12 a.txt
    typ "x"
    key C-q
    wait_for "1 file(s) not saved. Ctrl+Q again quits without saving."
    alive || fail "quit on the first Ctrl+Q"
    typ "y"                                  # disarms
    key C-q
    wait_for "1 file(s) not saved"
    alive || fail "quit after a disarming key"
    key C-q
    assert_exited
    assert_file a.txt $'A\n'
    start 100 12 a.txt                       # nothing unsaved: one Ctrl+Q quits
    key C-q
    assert_exited
}

t_crlf_kept() {
    printf 'one\r\ntwo\r\n' > c.txt
    start 100 12 c.txt
    status " CRLF "
    key End; status "Ln 1, Col 4 "
    key Right; status "Ln 2, Col 1 "         # \r\n is one step
    key Left;  status "Ln 1, Col 4 "
    key Enter; typ "mid"
    wait_line 2 "mid"
    key C-d                                  # duplicate line
    wait_line 3 "mid"
    key Up C-c Down C-v                      # whole-line copy/paste
    wait_line 3 "mid"
    key C-y Up Home BSpace                   # Backspace joins lines across \r\n
    wait_line 2 "midmid"
    screen | grep -q '?' && fail "a control character is shown"
    save
    assert_file c.txt $'one\r\nmidmid\r\ntwo\r\n'
}
