#!/usr/bin/perl
# A tiny language server for the e2e scenarios (scenarios/lsp.sh): canned answers, no real
# language behind it. Any file it is given: one error diagnostic on line 2, a completion
# "greet" (a snippet with two placeholders), signature help, usages, structure, symbols.
use strict;
use warnings;
use JSON::PP;
use Cwd qw(getcwd);

binmode STDIN;
binmode STDOUT;
$| = 1;
my $json = JSON::PP->new->canonical;

sub send_msg {
    my $body = $json->encode($_[0]);
    print "Content-Length: " . length($body) . "\r\n\r\n" . $body;
}

sub loc {
    my ($uri, $line, $col) = @_;
    return { uri => $uri, range => { start => { line => $line, character => $col }, end => { line => $line, character => $col + 5 } } };
}

my %text;
while (1) {
    my $len;
    while (my $h = <STDIN>) {
        $h =~ s/\r?\n$//;
        last if $h eq '' && defined $len;
        $len = $1 if $h =~ /^Content-Length:\s*(\d+)/i;
    }
    exit 0 unless defined $len;
    my $body = '';
    read(STDIN, $body, $len - length($body), length($body)) while length($body) < $len;
    my $m = $json->decode($body);
    my ($id, $method, $p) = ($m->{id}, $m->{method} // '', $m->{params});
    my $uri = $p && $p->{textDocument} ? $p->{textDocument}{uri} : '';
    my $reply = sub { send_msg({ jsonrpc => '2.0', id => $id, result => $_[0] }) };
    if ($method eq 'initialize') {
        $reply->({ capabilities => {
            textDocumentSync => 1,
            completionProvider => { triggerCharacters => ['.'] },
            signatureHelpProvider => { triggerCharacters => ['(', ','] },
            definitionProvider => JSON::PP::true, referencesProvider => JSON::PP::true,
            implementationProvider => JSON::PP::true, documentSymbolProvider => JSON::PP::true,
            workspaceSymbolProvider => JSON::PP::true,
        } });
    } elsif ($method eq 'textDocument/didOpen' || $method eq 'textDocument/didChange') {
        send_msg({ jsonrpc => '2.0', method => 'textDocument/publishDiagnostics', params => { uri => $uri, diagnostics => [
            { range => { start => { line => 1, character => 0 }, end => { line => 1, character => 3 } }, severity => 1, message => 'fake error on line 2', source => 'fake' },
            { range => { start => { line => 3, character => 0 }, end => { line => 3, character => 3 } }, severity => 2, message => 'fake warning on line 4', source => 'fake' },
        ] } });
    } elsif ($method eq 'textDocument/completion') {
        $reply->([{ label => 'greet', kind => 3, insertTextFormat => 2, insertText => 'greet(${1:name}, ${2:times})$0', detail => 'fn(name, times)' }]);
    } elsif ($method eq 'textDocument/signatureHelp') {
        $reply->({ signatures => [{ label => 'greet(name, times)', parameters => [{ label => 'name' }, { label => 'times' }] }], activeParameter => 0 });
    } elsif ($method eq 'textDocument/references') {
        $reply->([loc($uri, 0, 0), loc($uri, 2, 0)]);
    } elsif ($method eq 'textDocument/definition' || $method eq 'textDocument/implementation') {
        $reply->([loc($uri, 2, 0)]);
    } elsif ($method eq 'textDocument/documentSymbol') {
        $reply->([
            { name => 'first_thing', kind => 12, range => loc($uri, 0, 0)->{range}, selectionRange => loc($uri, 0, 0)->{range} },
            { name => 'third_thing', kind => 12, range => loc($uri, 2, 0)->{range}, selectionRange => loc($uri, 2, 0)->{range} },
        ]);
    } elsif ($method eq 'workspace/symbol') {
        my $q = $p->{query} // '';
        my $dir = getcwd();
        $reply->([grep { $_->{name} =~ /\Q$q\E/i } (
            { name => 'SomeWidget', kind => 5, location => loc("file://$dir/w.py", 0, 0) },
            { name => 'OtherThing', kind => 12, location => loc("file://$dir/w.py", 0, 0) },
        )]);
    } elsif ($method eq 'shutdown') {
        $reply->(undef);
    } elsif ($method eq 'exit') {
        exit 0;
    } elsif (defined $id) {
        $reply->(undef);
    }
}
