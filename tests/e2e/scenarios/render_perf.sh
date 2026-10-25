# Rendering of tabs and wide characters; a large file.

t_tabs_wide_chars() {
    printf 'ab\tX|\n\xe7\x95\x8c\tX|\n\t\tX|\n' > w.txt        # \xe7\x95\x8c = 界 (2 columns)
    start 100 12 w.txt
    wait_line 1 "ab→ X|"                     # tab runs to column 4 in both lines
    wait_line 2 "界→ X|"
    wait_line 3 "→   →   X|"
    key Down End
    status "Ln 2, Col 5 "
    cursor_at 11 2                           # gutter 5 + display column 6
    key Up;   status "Ln 1, Col 6 "          # same display column (end of line 1)
    cursor_at 11 1
    key Down Down; status "Ln 3, Col 2 "     # display column 6 is inside the 2nd tab
    key C-Home
    typ "界"
    wait_line 1 "界ab→   X|"                 # tab stop moves after the wide char
    cursor_at 7 1
    save
    assert_file w.txt $'\xe7\x95\x8cab\tX|\n\xe7\x95\x8c\tX|\n\t\tX|\n'
}

t_big_file_perf() {
    awk 'BEGIN { for (i = 1; i <= 20000; i++) printf "line %d: the quick brown fox jumps over the lazy dog\n", i }' > big.txt
    local t0 t1
    t0=$(now_ms)
    start 100 24 big.txt
    t1=$(now_ms); note "open $((t1 - t0)) ms"
    local slow=0
    measure() {   # measure <label> <key> <status text>
        local a b; a=$(now_ms); key "$2"; status "$3"; b=$(now_ms)
        note "$1 $((b - a)) ms"
        [ $((b - a)) -lt 500 ] || slow=1
    }
    measure PageDown NPage "Ln 22, Col 1 "   # 22-row text area: 21 lines per page
    measure Ctrl+End C-End "Ln 2000"          # 20000 or 20001, see t_ctrl_end_last_line
    wait_line 20000 "line 20000: the quick brown fox jumps over the lazy dog"
    measure Ctrl+Home C-Home "Ln 1, Col 1 "
    local a b; a=$(now_ms); repeat 100 NPage; status "Ln 2101, Col 1 "; b=$(now_ms)
    note "100xPageDown $((b - a)) ms"
    key C-End; typ "!"
    a=$(now_ms); save; b=$(now_ms); note "save $((b - a)) ms"
    [ "$(tail -c 1 big.txt)" = "!" ] || fail "the typed ! is not at the end of the saved file"
    grep -q "^line 19999: the quick brown fox jumps over the lazy dog$" big.txt || fail "content changed"
    [ $slow -eq 0 ] || fail "a single key took 500 ms or more"
}
