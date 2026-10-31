# Workspaces (decision 0013): several repos at once. Scratch repos only (alpha/, beta/ in the
# scratch dir); the remote is a bare repo in the scratch dir, never a real one.

# mk_repo <dir> <file> <text>: a repo with one committed file.
mk_repo() {
    mkdir -p "$1/$(dirname "$2")"
    printf '%s\n' "$3" > "$1/$2"
    ( cd "$1" && git init -q -b main . && git config user.name E2E && git config user.email e2e@example.com \
        && git config commit.gpgsign false && git add . && git commit -qm first ) || fail "repo $1 failed"
}

two_repos() {
    mk_repo alpha src/main.rs 'fn main() { needle(); }'
    printf 'alpha readme\n' > alpha/README.md; ( cd alpha && git add . && git commit -qm readme )
    mk_repo beta lib/util.rs 'pub fn needle() {}'
    printf 'beta readme\n' > beta/README.md; ( cd beta && git add . && git commit -qm readme )
}

t_ws_multi_root_tree() {
    two_repos
    printf 'changed\n' >> beta/lib/util.rs
    start 120 30 .
    wait_for "Workspace · "
    status "Workspace: 2 repos"
    wait_for "▾ alpha"
    wait_for "▾ beta"
    wait_for "●1"                                # beta's badge: one changed file
    wait_for "⎇main"
    key Down Down                               # in the tree: alpha › README.md ...
    key -                                       # collapse all
    wait_for "▸ alpha"
    wait_for "▸ beta"
    wait_gone "README.md"
    typ "bet"; key Enter                        # speed search across roots, Enter unfolds
    wait_for "▾ beta"
}

t_ws_cross_repo_search() {
    two_repos
    start 130 30 .
    wait_for "▾ beta"
    key Escape                                  # leave the tree (Ctrl+T there searches its folder)
    key C-t; typ "needle"
    wait_for "alpha › src/main.rs:1"
    wait_for "beta › lib/util.rs:1"
    key Escape
    key C-p; typ "@beta readme"
    wait_for "Find File · in beta"
    wait_for "README.md  beta"
    key Enter
    status "beta ⎇ main"
    wait_for "1  beta readme"
    key M-f; typ "@alpha needle"                # Find in Files in one repo
    wait_for "alpha › src/main.rs:1"
    wait_gone "beta › lib/util.rs:1"
}

t_ws_per_repo_git() {
    two_repos
    printf 'more\n' >> alpha/README.md
    printf 'x\n' > beta/new.txt
    start 130 30 alpha beta alpha/README.md
    wait_for "Workspace · alpha + 1 (2)"
    status "alpha ⎇ main"                       # the editor's file is in alpha
    key C-p; typ "util"; key Enter
    status "beta ⎇ main"                        # the current repo follows the editor
    key C-o
    wait_for "Repos · 2"
    wait_for "1 file"
    wait_for "+1 −0"
    key Up                                      # alpha (beta is current)
    key c                                       # alpha's Commit panel
    wait_for "alpha · Changes 1"
    wait_for "M README.md"
    key Escape
    key C-l
    wait_for "alpha ⎇ main"                     # the Git view shows alpha
    wait_for "+ more"
    key o; wait_for "Repos · 2"; key Down; key g
    wait_for "beta ⎇ main"
    wait_for "? new.txt"
}

t_ws_fetch_all_confirm() {
    two_repos
    git init -q --bare -b main remote.git
    ( cd alpha && git remote add origin ../remote.git && git push -q -u origin main ) || fail "push failed"
    git clone -q remote.git other && ( cd other && git config user.name E2E && git config user.email e2e@example.com \
        && printf 'new\n' > n.txt && git add . && git commit -qm upstream && git push -q ) || fail "upstream commit failed"
    start 130 30 alpha beta
    key C-o
    wait_for "✓ in sync"
    key F
    wait_for "Fetch alpha from their remotes, 1 skipped? Nothing local changes"
    key n
    status "Cancelled"
    key F; key y
    status "Fetched 1 of 1 repo · 1 skipped"
    wait_for "↑0 ↓1"                            # alpha is behind now
    wait_for "skipped: no remote"
    key U
    wait_for "Pull alpha, fast-forward only"
    key y
    status "Pulled 1 of 1 repo"
    [ -f alpha/n.txt ] || fail "pull didn't bring n.txt"
    wait_for "✓ in sync"
}

t_ws_add_root_file_and_recent() {
    two_repos
    mk_repo gamma g.txt 'gamma'
    cat > studio.catlas.toml <<'EOF'
name = "Studio"
[[root]]
path = "alpha"
label = "front"
[[root]]
path = "beta"
EOF
    start 130 30 studio.catlas.toml
    wait_for "Workspace · Studio (2)"
    wait_for "▾ front"
    key C-o
    key a; typ "gamma"; key Enter
    status "Added gamma · 3 roots now"
    wait_for "Repos · 3"
    grep -q 'label = "gamma"' "$CFG/workspaces.toml" || fail "recent list not saved: $(cat "$CFG/workspaces.toml" 2>/dev/null)"
    key d                                       # remove gamma again (nothing on disk changes)
    status "Removed gamma from the workspace"
    [ -d gamma/.git ] || fail "gamma was touched"
    key w
    wait_for "Studio"
    key Escape; key Escape
    wait_gone "Repos · "
    CATLAS_CONFIG_DIR="$CFG" "$CATLAS" -w | grep -q "Studio" || fail "catlas -w doesn't list it"
}

t_ws_single_root_unchanged() {
    mk_repo solo a.txt 'one'
    start 100 24 solo
    wait_for " Project"
    wait_gone "Workspace"
    status " ⎇ main"
    key C-p; typ "a.txt"
    wait_for "a.txt"
}
