# Perl 绑定测试：用核心模块 Test::More，不联网。
use strict;
use warnings;
use FindBin qw($Bin);
use lib "$Bin/..";
use Test::More;
use Pagesift;

my $v = Pagesift::version();
diag "libpagesift $v";
like $v, qr/^\d+\.\d+\.\d+/, 'version looks like x.y.z';

my $md = Pagesift::convert('<h1>Hi</h1><p>World</p>');
like $md, qr/"success":true/, 'convert succeeds';
like $md, qr/# Hi/, 'convert keeps the heading';

my $e = Pagesift::engine();
my $doc = $e->parse("name,score\nalice,1\nbob,2\n", 'scores.csv');
like $doc, qr/"success":true/, 'parse csv succeeds';
like $doc, qr/\| alice \| 1 \|/, 'parse csv renders a table row';
my $bad = $e->parse("\0\1\2\3", 'blob.xyz');
like $bad, qr/"success":false/, 'unsupported file type reports success=false';
like $bad, qr/"error_code":"UNSUPPORTED_OPTION"/, 'unsupported file type error_code';
like $e->scrape('not a url'), qr/"error_code":"INVALID_REQUEST"/, 'bad url error_code';
$e->close;
ok !eval { $e->scrape('http://example.invalid/'); 1 }, 'closed engine raises';

ok !eval { Pagesift::engine('{"no_such_key":true}'); 1 }, 'engine create rejects unknown config key';
ok length($@), 'rejection carries a reason';

done_testing;
