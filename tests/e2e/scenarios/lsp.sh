# LSP features against a fake language server (tests/e2e/fake-lsp.pl, canned answers), set up
# through a project .code-atlas/languages.toml: config override, --health, diagnostics + F2,
# snippet completion with Tab stops, signature help, usages / structure / symbol pickers.

fake_lsp_project() {
    mkdir -p .code-atlas
    cat > .code-atlas/languages.toml <<TOML
[language.python]
servers = ["fake"]
also = []

[language-server.fake]
command = "$E2E_DIR/fake-lsp.pl"
version-args = []
TOML
    printf 'one = 1\ntwo = 2\nthree = 3\nfour = 4\n\n' > w.py
}

t_lsp_health() {
    fake_lsp_project
    "$CATLAS" --health . > health.txt 2>&1 || fail "--health exited with $?"
    grep -q "\.code-atlas/languages.toml (loaded)" health.txt || fail "project config not loaded: $(head -8 health.txt)"
    grep -E "^python +yes +fake +✓ used +$E2E_DIR/fake-lsp.pl" health.txt >/dev/null || fail "python row: $(grep '^python' health.txt)"
    grep -qE "^rust +yes +rust-analyzer" health.txt || fail "rust row missing"
    printf '[language.go]\nservers = "gopls"\n' > .code-atlas/languages.toml
    "$CATLAS" --health . > health.txt 2>&1
    grep -q "ERROR, ignored: TOML parse error" health.txt || fail "bad config not reported: $(head -8 health.txt)"
}

t_lsp_diagnostics_f2() {
    fake_lsp_project
    start 100 20 w.py
    wait_for "●"
    key F2
    wait_for "1/2 fake error on line 2"
    status "Ln 2, Col 1 "
    key F2
    wait_for "2/2 fake warning on line 4"
    key S-F2
    wait_for "1/2 fake error on line 2"
    key M-6
    wait_for "Problems in w.py (2)"
    key Down Enter
    status "Ln 4, Col 1 "
}

t_lsp_snippet_and_signature() {
    fake_lsp_project
    start 100 20 w.py
    wait_for "●"
    key C-End
    typ "gr"; key C-Space
    wait_for "greet"
    key Enter
    wait_line 6 "greet(name, times)"
    status "(4 chars)"                              # the first placeholder is selected
    wait_for "greet(name, times)" 5                 # signature help (above the line)
    typ "a"; key Tab
    status "(5 chars)"
    typ "3"; key Tab
    typ "!"
    wait_line 6 "greet(a, 3)!"
}

t_lsp_pickers() {
    fake_lsp_project
    start 100 20 w.py
    wait_for "●"
    key M-F7
    wait_for "Usages of one · 2 in 1 file(s)"
    key Down Enter
    status "Ln 3, Col 1 "
    key C-F12
    wait_for "Structure of w.py"
    typ "fir"
    wait_gone "third_thing"
    key Enter
    status "Ln 1, Col 1 "
    key M-n
    wait_for "Go to symbol in the project"
    key C-u; typ "Widget"
    wait_for "SomeWidget"
    wait_gone "OtherThing"
    key Escape
    wait_gone "Go to symbol"
    key C-b
    status "Ln 3, Col 1 "
}
