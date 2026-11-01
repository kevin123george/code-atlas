# Decision 0019: Enter per language, on the real binary.

t_enter_markdown_list() {
    : > n.md
    start 100 20 n.md
    typ "- one"; key Enter
    typ "two"; key Enter
    key Enter                                   # an empty item ends the list
    typ "end"
    save
    assert_file n.md $'- one\n- two\nend'
}

t_enter_markdown_numbers_and_tasks() {
    : > n.md
    start 100 20 n.md
    typ "1. a"; key Enter
    typ "b"; key Enter; key Enter
    typ "- [x] done"; key Enter
    typ "next"
    save
    assert_file n.md $'1. a\n2. b\n- [x] done\n- [ ] next'
}

t_enter_yaml_after_key() {
    : > n.yml
    start 100 20 n.yml
    typ "services:"; key Enter
    typ "api:"; key Enter
    typ "image: x"
    save
    assert_file n.yml $'services:\n  api:\n    image: x'
}

t_enter_shell_blocks() {
    : > n.sh
    start 100 20 n.sh
    typ "if true; then"; key Enter
    typ "echo a"; key Enter
    typ "fi"; key Enter
    typ "echo b"
    save
    assert_file n.sh $'if true; then\n  echo a\nfi\necho b'
}

t_markdown_bold_is_bold() {
    printf 'plain **loud** text\n' > n.md
    start 100 20 n.md
    wait_for "loud"
    # The codes just before the word (tmux may send bold and the colour as separate sequences).
    tm capture-pane -p -e -t "$SESSION" | perl -ne '$f=1 if /((?:\e\[[0-9;:]*m)+)loud/ && $1 =~ /\e\[(?:[0-9]+;)*1(?:;[0-9]+)*m/; END { exit($f ? 0 : 1) }' || fail "loud is not bold"
}
