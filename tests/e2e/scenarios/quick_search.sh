# Search Everywhere (Ctrl+T, decision 0009): file names + contents in one popup, tabs, name:42,
# the tree following the selection, scoped search from the tree, and the tree's speed search.

quick_project() {
    mkdir -p src/deep/inner docs secret
    printf 'fn main() {}\nlet beta = 1;\n' > src/alpha.rs
    printf 'pub fn z() {}\n' > src/deep/inner/zeta.rs
    printf 'alpha here\n' > docs/notes.md
    printf 'alpha secret\n' > secret/alpha.txt
    printf 'secret/\nbin/\n' > .gitignore
}

t_quick_mixed_names_and_text() {
    quick_project
    start 120 30
    key C-t
    wait_for "Search Everywhere"
    typ "alpha"
    wait_for "FILES  1"
    wait_for "alpha.rs  src"
    wait_for "TEXT  1 in 1 file"
    wait_for "docs/notes.md:1  alpha here"
    screen | grep -q "secret/alpha" && fail "a gitignored file was searched"
    key Enter                                # first row: the file
    wait_for "▎alpha.rs"
    wait_gone "Search Everywhere"
}

t_quick_text_tab_opens_at_match() {
    quick_project
    start 120 30
    key C-t; wait_for "Search Everywhere"
    typ "beta"
    wait_for "src/alpha.rs:2  let beta = 1"
    key Tab                                  # Files tab: no text rows
    wait_gone "src/alpha.rs:2"
    key Tab                                  # Text tab
    wait_for "src/alpha.rs:2"
    key Enter
    wait_for "▎alpha.rs"
    status "Ln 2, Col 9 "                    # at the end of the selected match
}

t_quick_name_line_suffix() {
    quick_project
    start 120 30
    key C-t; wait_for "Search Everywhere"
    typ "alpha:2"
    wait_for "alpha.rs  src"
    key Enter
    wait_for "▎alpha.rs"
    status "Ln 2, Col 1 "
}

t_quick_tree_follows_and_esc_restores() {
    quick_project
    start 120 30
    wait_for "▸ src"
    key C-t; wait_for "Search Everywhere"
    typ "zeta"
    wait_for "zeta.rs  src/deep/inner"
    wait_for "▾ inner"                        # the tree opened down to the result
    key Escape
    wait_gone "Search Everywhere"
    wait_gone "inner"                         # ...and closed again on Esc
    wait_for "▸ src"
}

t_quick_alt_f1_reveals_in_tree() {
    quick_project
    start 120 30
    key C-t; wait_for "Search Everywhere"
    typ "zeta"
    wait_for "zeta.rs  src/deep/inner"
    key M-F1
    wait_gone "Search Everywhere"
    status "Project:"                         # the tree has the focus
    key Enter
    wait_for "▎zeta.rs"
}

t_quick_scoped_from_tree() {
    quick_project
    start 120 30
    wait_for "▸ docs"
    typ "docs"                                # speed search selects the folder
    wait_for "Search: docs"
    key C-t
    wait_for "Search Everywhere · in docs/"
    typ "alpha"
    wait_for "docs/notes.md:1"
    screen | grep -q "src/alpha.rs" && fail "the scoped search left docs/"
    key C-u BSpace                            # Backspace on an empty input: whole project
    wait_gone "in docs/"
    typ "alpha"
    wait_for "alpha.rs  src"
}

t_tree_speed_search() {
    quick_project
    start 120 30
    wait_for "▸ src"
    typ "zet"                                 # not among the open rows: found in the index
    wait_for "Search: zet"
    wait_for "zeta.rs"
    status "Speed search"
    key BSpace; wait_for "Search: ze"
    key Escape
    wait_gone "Search: "
    wait_for " Project"
    typ "zeta"; key Enter
    wait_for "▎zeta.rs"
}

t_quick_meta_and_mac_keys() {
    quick_project
    start 120 30
    key M-s; wait_for "Search Everywhere"
    key Escape; wait_gone "Search Everywhere"
    key M-Space; wait_for "Search Everywhere"
    key Escape; wait_gone "Search Everywhere"
    raw c2 a0                                 # macOS Option+Space without "Option as Meta"
    wait_for "Search Everywhere"
}
