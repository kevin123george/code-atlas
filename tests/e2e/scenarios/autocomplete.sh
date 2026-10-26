# Completion as you type (decision 0011): the popup opening by itself with the fake language
# server (tests/e2e/fake-lsp.pl, see lsp.sh), buffer words without a server, the Mac fallback
# keys Ctrl+J and Ctrl+@ (NUL), and `autocomplete = false`.

t_auto_popup_server() {
    fake_lsp_project
    start 100 20 w.py
    wait_for "●"
    key C-End
    typ "gr"                                   # no key: the list opens after a pause
    wait_for "fn(name, times)"
    key Enter
    wait_line 6 "greet(name, times)"
    status "(4 chars)"                         # snippet placeholder selected
    key Escape
    # A trigger character (the server says "."): opens at once.
    key End Enter
    typ "one."
    wait_for "fn(name, times)"
    key Escape
    wait_gone "fn(name, times)"
    typ "gr"                                   # Esc closed it for this word: it stays shut
    sleep 0.4
    screen | grep -qF "fn(name, times)" && fail "popup came back after Esc"
    # Few server matches: buffer words are added under them ("three" from the file).
    key Enter
    typ "thr"
    wait_for "w   three"
    key Tab
    wait_line 8 "three"
}

t_auto_words_without_server() {
    printf 'select customer_name from customers;\n' > q.sql      # SQL: no language server
    start 100 20 q.sql
    key C-End
    typ "cus"
    wait_for "w   customers"
    screen | grep -qF "w   customer_name" || fail "both words offered"
    typ "tomer_"
    wait_gone "w   customers"
    key Enter
    wait_line 2 "customer_name"
    save
    assert_file q.sql $'select customer_name from customers;\ncustomer_name'
}

t_complete_fallback_keys() {
    printf 'elephant\n' > notes.txt
    start 100 20 notes.txt
    key C-End
    typ "ele"
    sleep 0.4                                  # plain text: no popup by itself
    screen | grep -qF "w   elephant" && fail "plain text popped up by itself"
    key C-j                                    # Ctrl+J: works in Terminal.app without setup
    wait_for "w   elephant"
    key Enter
    wait_line 2 "elephant"
    key Enter
    typ "el"
    raw 00                                     # Ctrl+Space / Ctrl+@ as most terminals send it (NUL)
    wait_for "w   elephant"
    key Escape
    wait_gone "w   elephant"
}

t_autocomplete_setting_off() {
    printf 'select customer_name from customers;\n' > q.sql
    CFG="$SCRATCH/bin/.catlas-config"; mkdir -p "$CFG"
    printf 'hints = false\nautocomplete = false\n' > "$CFG/settings.toml"
    start 100 20 q.sql
    key C-End
    typ "cus"
    sleep 0.4
    screen | grep -qF "w   customers" && fail "popup with autocomplete = false"
    key C-j
    wait_for "w   customers"
}
