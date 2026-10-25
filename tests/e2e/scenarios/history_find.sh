# Undo/redo, find, go to line.

t_undo_redo() {
    printf 'x\n' > u.txt
    start 100 12 u.txt
    typ "abc def"
    wait_line 1 "abc defx"
    wait_for "u.txt ●"
    key C-z; wait_line 1 "abc x"             # typing groups: "def", " ", "abc"
    key C-z; wait_line 1 "abcx"
    key C-z; wait_line 1 "x"
    wait_gone "●"                            # back at the saved state: not dirty
    key C-z; wait_line 1 "x"                 # nothing more to undo
    key M-z; wait_line 1 "abcx"              # Alt+Z: redo fallback for Ctrl+Shift+Z
    key M-z; wait_line 1 "abc x"
    wait_for "u.txt ●"
    typ "!"                                  # a new edit clears the redo stack
    key M-z; wait_line 1 "abc !x"
    save
    assert_file u.txt $'abc !x\n'
}

t_find_f3() {
    printf 'foo bar\nFoo baz\nfoo qux\n' > f.txt
    start 100 12 f.txt
    key C-f
    wait_for "Find: "
    typ "foo"; key Enter
    status "Ln 1, Col 4 (3 chars)"
    wait_for "F3 next"
    key F3;   status "Ln 2, Col 4 (3 chars)" # lowercase query ignores case
    key F3;   status "Ln 3, Col 4 (3 chars)"
    key F3;   status "Ln 1, Col 4 (3 chars)" # wraps around
    key F3;   status "Ln 2, Col 4 (3 chars)"
    key C-f                                  # the selection fills the prompt
    wait_for "Find: Foo"
    repeat 3 BSpace; typ "Foo"; key Enter    # capital letter: case-sensitive
    key F3;   status "Ln 2, Col 4 (3 chars)"
    key C-f; repeat 3 BSpace; typ "zzz"; key Enter
    wait_for "Not found: zzz"
    key C-f; wait_for "Find: "; key Escape
    wait_gone "Find: "
}

t_goto_line() {
    printf 'l1\nline2\nabcdef\nl4\nl5\n' > g.txt
    start 100 12 g.txt
    key C-g
    wait_for "Go to line[:column]: "
    typ "3:2"; key Enter
    status "Ln 3, Col 2 "
    key C-g; typ "99"; key Enter             # past the end: last line (the empty one after the final break)
    status "Ln 6, Col 1 "
    key C-g; typ "2:99"; key Enter           # past the line end: end of line
    status "Ln 2, Col 6 "
    key C-g; typ "x"; key Enter
    wait_for "Type a line number"
}

# Find previous. Terminals without the kitty protocol (tmux, Terminal.app) send Shift+F3 as ESC[1;2R,
# which crossterm 0.28 reads as a cursor-position report and drops, so Alt+P is the fallback.
# (Shift+F3 itself is covered by the kitty-bytes scenario.)
t_find_prev_shift_f3() {
    printf 'foo bar\nfoo baz\nfoo qux\n' > f.txt
    start 100 12 f.txt
    key C-f; typ "foo"; key Enter
    status "Ln 1, Col 4 (3 chars)"
    key M-p; status "Ln 3, Col 4 (3 chars)"
}

# Keys tmux can't send (it turns Ctrl+Shift+Z into Ctrl+Z, Shift+F3 into ESC[1;2R): inject the bytes
# a kitty-protocol terminal (Ghostty, kitty, WezTerm, iTerm2 CSI u) sends, to check the key map.
t_kitty_ctrl_shift_z_redo() {
    printf 'x\n' > k.txt
    start 100 12 k.txt
    typ "ab"; wait_line 1 "abx"
    key C-z; wait_line 1 "x"
    raw 1b 5b 31 32 32 3b 36 75              # ESC[122;6u = Ctrl+Shift+Z
    wait_line 1 "abx"
}

t_kitty_shift_f3_previous() {
    printf 'foo bar\nfoo baz\nfoo qux\n' > f.txt
    start 100 12 f.txt
    key C-f; typ "foo"; key Enter
    status "Ln 1, Col 4 (3 chars)"
    raw 1b 5b 31 33 3b 32 7e                 # ESC[13;2~ = Shift+F3
    status "Ln 3, Col 4 (3 chars)"
}
