# XML: highlighting (colours read from the tmux screen with escape sequences) and Ctrl+/ comments.

# colour_before <r;g;b> <text>: until <text> is drawn with that foreground (24-bit, as ui::theme
# sends it). On a timeout the coloured screen goes to out/<scenario>.ansi.txt.
colour_before() {
    local rgb="$1" text="$2" end
    end=$(( $(now_ms) + TIMEOUT * 1000 ))
    while :; do
        # Replays the SGR codes per line, keeping the foreground of every character, and looks
        # for <text> drawn entirely in <rgb>.
        tm capture-pane -e -p -t "$SESSION" 2>/dev/null | RGB="$rgb" TEXT="$text" perl -CS -ne '
            my ($plain, @fg, $fg) = ("");
            for my $part (split /(\e\[[0-9;]*m)/) {
                if ($part =~ /^\e\[([0-9;]*)m$/) {
                    my @c = split /;/, $1;
                    while (@c) {
                        my $c = shift @c;
                        if ($c eq "" || $c == 0 || $c == 39) { $fg = "" }
                        elsif ($c == 38 && @c >= 4 && $c[0] == 2) { shift @c; $fg = join ";", splice(@c, 0, 3) }
                        elsif (($c == 48 || $c == 58) && @c && $c[0] == 2) { splice(@c, 0, 4) }
                        elsif (($c == 48 || $c == 58) && @c && $c[0] == 5) { splice(@c, 0, 2) }
                    }
                } else { $plain .= $part; push @fg, ($fg) x length $part }
            }
            my $t = $ENV{TEXT}; utf8::decode($t);
            my $i = -1;
            while (($i = index($plain, $t, $i + 1)) >= 0) {
                if (!grep { ($_ // "") ne $ENV{RGB} } @fg[$i .. $i + length($t) - 1]) { $found = 1; last }
            }
            END { exit !$found }' && return 0
        if [ "$(now_ms)" -ge "$end" ]; then
            mkdir -p "$OUT"; tm capture-pane -e -p -t "$SESSION" > "$OUT/$SCEN.ansi.txt" 2>/dev/null
            fail "never drawn in $rgb: $text"
        fi
        sleep 0.05
    done
}

t_xml_highlight() {
    printf '<?xml version="1.0"?>\n<!-- build -->\n<project lang="en">\n  <name>Atlas</name>\n</project>\n' > pom.xml
    start 100 12 pom.xml
    status "XML"
    colour_before "240;180;41" "project"          # tag: S_YELLOW
    colour_before "242;138;181" "lang"            # attribute name: S_MAGENTA (as in HTML)
    colour_before "51;214;160" '"en"'             # attribute value: S_AQUA
    colour_before "92;101;119" "<!-- build -->"   # comment: MUTED (italic)
    colour_before "51;188;219" "xml"              # <?xml: keyword S_TEAL
    colour_before "232;236;245" "Atlas"           # text: plain TEXT
}

t_xml_comment_toggle() {
    printf '<r>\n  <a x="1"/>\n  <b/>\n</r>\n' > c.xml
    start 100 12 c.xml
    status "XML"
    key Down
    key C-_                                       # Ctrl+/ (tmux sends it as Ctrl+_)
    wait_line 2 '  <!-- <a x="1"/> -->'
    status "Ln 3, Col 1"                          # the cursor moves on, as in IntelliJ
    key Up S-Down S-End                           # select lines 2-3
    key C-_
    wait_line 2 '  <!-- <a x="1"/> -->'           # mixed: the commented line stays, <b/> gets wrapped
    wait_line 3 '  <!-- <b/> -->'
    key C-_                                       # all commented: unwrap both
    wait_line 2 '  <a x="1"/>'
    wait_line 3 '  <b/>'
    key C-z
    wait_line 3 '  <!-- <b/> -->'
    save
    assert_file c.xml $'<r>\n  <!-- <a x="1"/> -->\n  <!-- <b/> -->\n</r>\n'
}
