# Line operations: Ctrl+D, Ctrl+Y, Alt+Shift+Up/Down, Tab / Shift+Tab.

t_duplicate() {
    printf 'a\nb\n' > d.txt
    start 100 12 d.txt
    key C-d
    wait_line 2 "a"
    status "Ln 2, Col 1 "
    key Down End S-Home C-d                  # duplicate a selection
    wait_line 3 "bb"
    status "(1 chars)"
    save
    assert_file d.txt $'a\na\nbb\n'
}

t_delete_line() {
    printf '1\n2\n3\n' > y.txt
    start 100 12 y.txt
    key Down C-y
    wait_line 2 "3"
    status "Ln 2, Col 1 "
    save
    assert_file y.txt $'1\n3\n'
    key C-y                                  # the last line: the file keeps its final newline
    wait_gone "  2  3"
    save
    assert_file y.txt $'1\n'
}

t_delete_last_line_no_eol() {
    printf '1\n2' > y.txt
    start 100 12 y.txt
    key Down C-y
    wait_gone "  2  2"
    save
    assert_file y.txt '1'
}

t_move_lines() {
    printf 'a\nb\nc\n' > m.txt
    printf 'x\ny' > n.txt
    start 100 12 m.txt n.txt
    key M-S-Down
    wait_line 2 "a"
    status "Ln 2, Col 1 "
    key M-S-Down
    wait_line 3 "a"
    key M-S-Down                             # already last: nothing happens
    key M-S-Up
    wait_line 2 "a"
    key C-Home S-Down S-Down                 # lines 1-2 selected (ends at start of line 3)
    key M-S-Down
    wait_line 1 "c"
    wait_line 2 "b"
    wait_line 3 "a"
    save
    assert_file m.txt $'c\nb\na\n'
    key M-Right                              # second tab: no final newline
    wait_for "▎n.txt"
    key Down M-S-Up
    wait_line 1 "y"
    save
    assert_file n.txt $'y\nx'
}

t_tab_untab() {
    printf 'a\nb\nc\n' > t.txt
    start 100 12 t.txt
    key S-Down S-Down Tab
    wait_line 1 "    a"
    wait_line 2 "    b"
    wait_line 3 "c"
    status "chars)"                          # still a selection
    key BTab                                 # Shift+Tab arrives as BackTab
    wait_line 1 "a"
    wait_line 2 "b"
    key BTab                                 # nothing left to unindent
    wait_line 1 "a"
    key Escape End Tab                       # no selection: spaces to the next tab stop
    status "Ln 3, Col 5 "
    save
    assert_file t.txt $'a\nb\nc   \n'
}

# Ctrl+End in a file that ends with a line break. The editor hides the empty line after the final
# break (Doc::visible_lines), so the cursor should stay on a line it draws, as Down does.
t_ctrl_end_last_line() {
    printf 'a\nb\n' > e.txt
    start 100 12 e.txt
    key Down Down
    status "Ln 3, Col 1 "                    # as in IntelliJ: the line after a final break is drawn and reachable
    key C-Home C-End
    status "Ln 3, Col 1 "
}
