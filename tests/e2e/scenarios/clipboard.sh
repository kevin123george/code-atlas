# Ctrl+C / Ctrl+X / Ctrl+V. The system clipboard copy goes to the fake pbcopy ($SCRATCH/clipboard).

t_copy_paste_selection() {
    printf 'alpha beta\n' > c.txt
    start 100 12 c.txt
    repeat 5 S-Right
    status "(5 chars)"
    key C-c
    assert_clipboard "alpha"
    key End C-v
    wait_line 1 "alpha betaalpha"
    save
    assert_file c.txt $'alpha betaalpha\n'
}

t_copy_line_paste_above() {
    printf 'one\ntwo\nthree\n' > c.txt
    start 100 12 c.txt
    key C-c                                  # no selection: copies the whole line
    assert_clipboard $'one\n'
    key Down Down Right
    status "Ln 3, Col 2 "
    key C-v                                  # whole line goes above the current line
    wait_line 3 "one"
    wait_line 4 "three"
    status "Ln 4, Col 2 "                    # cursor stays in its text
    save
    assert_file c.txt $'one\ntwo\none\nthree\n'
}

t_cut_line() {
    printf 'one\ntwo\nthree\n' > c.txt
    start 100 12 c.txt
    key Down C-x
    wait_line 2 "three"
    assert_clipboard $'two\n'
    key Up C-v
    wait_line 1 "two"
    wait_line 2 "one"
    key End S-Left S-Left C-x                # cut a selection
    wait_line 2 "o"
    assert_clipboard "ne"
    save
    assert_file c.txt $'two\no\nthree\n'
}
