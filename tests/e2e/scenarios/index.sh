# The code index (decision 0014): SYMBOLS in Search Everywhere, Alt+N without a language server,
# new files showing up while catlas runs (watcher), the cache folder, --index-status, large files.
# The cache goes to $CACHE ($SCRATCH/bin/.catlas-cache, set by `start`), never ~/Library/Caches.

index_project() {
    mkdir -p src docs
    printf 'pub fn parse_config() {}\npub struct ConfigLoader;\n' > src/lib.rs
    printf 'fn main() { parse_config(); }\n' > src/main.rs
    printf '# Notes\nparse_config is described here\n' > docs/notes.md
    printf 'bin/\n' > .gitignore
}

t_index_symbols_in_search_everywhere() {
    index_project
    start 120 30
    sleep 0.5                                # the first index build (a few files)
    key C-t; wait_for "Search Everywhere"
    typ "parse_conf"
    wait_for "SYMBOLS  1"
    wait_for "fn   parse_config"
    wait_for "src/lib.rs:1"
    key Tab Tab                              # Files → Symbols
    wait_gone "TEXT"
    key Enter
    wait_for "▎lib.rs"
    status "Ln 1, Col 20 (12 chars)"         # the name selected
}

t_index_alt_n_without_a_server() {
    index_project
    start 120 30 docs/notes.md               # Markdown: no language server here
    sleep 0.5
    key M-n
    wait_for "Go to symbol in the project"
    key C-u; typ "ConfLoad"
    wait_for "ConfigLoader"
    key Enter
    wait_for "▎lib.rs"
    status "Ln 2, Col 12 "
}

t_index_sees_new_files() {
    index_project
    start 120 30
    sleep 0.8
    printf 'fn brand_new_fn() {}\n' > src/fresh_file_xyz.rs
    sleep 1.5                                # FSEvents latency + the worker's burst wait
    key C-p; wait_for "Find File"
    typ "fresh_file_x"
    wait_for "fresh_file_xyz.rs" 8
    key Escape
    key C-t; typ "brand_new"
    wait_for "fn   brand_new_fn" 8
    [ -f "$(ls -d "$CACHE"/index/*/manifest.json | head -1)" ] || fail "no index cache in $CACHE"
}

t_index_status_cli() {
    index_project
    start 100 30
    sleep 0.8
    key C-q
    assert_exited
    local out
    out=$(CATLAS_CONFIG_DIR="$CFG" CATLAS_CACHE_DIR="$CACHE" "$CATLAS" --index-status . 2>&1)
    echo "$out" | grep -q "files    4 " || fail "status: $out"
    echo "$out" | grep -q "0 new or changed" || fail "status: $out"
}

t_large_file_mode() {
    perl -e 'print "fn a() {}\n"; print "x" x 30000, "\n"' > min.rs
    start 120 30 min.rs
    wait_for "Large file"
    typ "zz"
    wait_line 1 "zzfn a() {}"
}
