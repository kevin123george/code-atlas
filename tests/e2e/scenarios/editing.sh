# Typing, pairing, smart Enter/Home, selection. Sourced by ../run.sh; each t_<name> is one scenario.

t_typing() {
    start 100 12 a.txt                       # a file that doesn't exist yet
    typ "hello world"
    wait_line 1 "hello world"
    status "Ln 1, Col 12 "
    wait_for "a.txt ●"                       # dirty marker in the tab
    repeat 5 BSpace
    typ "there"
    wait_line 1 "hello there"
    key C-h                                  # Ctrl+Backspace fallback: delete word left
    wait_line 1 "hello"
    status "Ln 1, Col 7 "                    # "hello " (the screen trims trailing spaces)
    typ "atlas"
    key Home Delete
    wait_line 1 "ello atlas"
    save
    wait_gone "●"
    assert_file a.txt "ello atlas"
}

t_pair_brackets() {
    start 100 12 p.rs
    typ "f("
    wait_line 1 "f()"
    status "Ln 1, Col 3 "
    typ "x)"                                 # ")" skips over the auto-inserted one
    wait_line 1 "f(x)"
    status "Ln 1, Col 5 "
    typ "["
    wait_line 1 "f(x)[]"
    key BSpace                               # Backspace between a pair removes both
    wait_line 1 "f(x)"
    typ "{}"
    wait_line 1 "f(x){}"
    status "Ln 1, Col 7 "
    key Home
    typ "("                                  # before a word: no closing bracket
    wait_line 1 "(f(x){}"
    save
    assert_file p.rs "(f(x){}"
}

t_pair_quotes() {
    start 100 12 q.rs
    typ 'let s = "hi";'
    wait_line 1 'let s = "hi";'
    key Enter
    typ "don't"                              # apostrophe after a letter doesn't pair
    wait_line 2 "don't"
    key Enter
    typ "'a'"
    wait_line 3 "'a'"
    status "Ln 3, Col 4 "
    save
    assert_file q.rs $'let s = "hi";\ndon\'t\n\'a\''
}

t_wrap_selection() {
    printf 'abc def\nxyz\n' > w.txt
    start 100 12 w.txt
    key S-Right S-Right S-Right              # select "abc"
    status "(3 chars)"
    typ "("
    wait_line 1 "(abc) def"
    status "(3 chars)"                       # inner text stays selected
    typ '"'                                  # a quote wraps the selection too (IntelliJ)
    wait_line 1 '("abc") def'
    key Down Home S-Right S-Right            # select "xy" inside the word "xyz"
    typ "["
    wait_line 2 "[xy]z"
    save
    assert_file w.txt $'("abc") def\n[xy]z\n'
}

t_smart_enter() {
    start 100 12 s.rs
    typ "fn x() {"
    wait_line 1 "fn x() {}"
    key Enter
    wait_line 1 "fn x() {"
    wait_line 3 "}"
    status "Ln 2, Col 5 "
    typ "a"
    key Enter                                # keeps the indent
    typ "b"
    wait_line 3 "    b"
    save
    assert_file s.rs $'fn x() {\n    a\n    b\n}'
}

t_home_toggle() {
    printf '    foo bar\n' > h.txt
    start 100 12 h.txt
    key End;  status "Ln 1, Col 12 "
    key Home; status "Ln 1, Col 5 "          # first non-blank
    key Home; status "Ln 1, Col 1 "          # then column 1
    key Home; status "Ln 1, Col 5 "
    key End S-Home
    status "Ln 1, Col 5 (7 chars)"
}

t_shift_select_type_over() {
    printf 'hello world\n' > s.txt
    start 100 12 s.txt
    repeat 5 S-Right
    status "(5 chars)"
    typ "bye"
    wait_line 1 "bye world"
    key Home S-Right S-Right S-Right
    status "(3 chars)"
    typ "hi"
    wait_line 1 "hi world"
    key End S-Left S-Left S-Left
    status "(3 chars)"
    key BSpace
    wait_line 1 "hi wo"
    save
    assert_file s.txt $'hi wo\n'
}

t_select_all() {
    printf 'one\ntwo\n' > a.txt
    start 100 12 a.txt
    key C-a
    status "(8 chars)"
    typ "x"
    wait_line 1 "x"
    save
    assert_file a.txt "x"
}
