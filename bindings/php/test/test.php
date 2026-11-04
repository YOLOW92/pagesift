<?php

// libpagesift PHP 绑定测试：不用 PHPUnit，每项检查打印结果，有失败就非零退出。不联网。

declare(strict_types=1);

require __DIR__ . '/../src/Pagesift.php';

use Pagesift\Engine;
use Pagesift\Pagesift;

$passed = 0;
$failed = 0;
$check = function (bool $ok, string $what) use (&$passed, &$failed): void {
    if ($ok) {
        $passed++;
        echo "  ok   {$what}\n";
    } else {
        $failed++;
        echo "  FAIL {$what}\n";
    }
};

$version = Pagesift::version();
echo "libpagesift {$version}\n";
$check(count(explode('.', $version)) === 3, 'version looks like x.y.z');

$md = Pagesift::convert('<h1>Hi</h1><p>World</p>');
$check(str_contains($md, '"success":true'), 'convert succeeds');
$check(str_contains($md, '# Hi'), 'convert keeps the heading');

$engine = Engine::create();
$csv = "name,score\nalice,1\nbob,2\n";
$doc = $engine->parse($csv, 'scores.csv');
$check(str_contains($doc, '"success":true'), 'parse csv succeeds');
$check(str_contains($doc, '| alice | 1 |'), 'parse csv renders a table row');

$bad = $engine->parse("\x00\x01\x02\x03", 'blob.xyz');
$check(str_contains($bad, '"success":false'), 'unsupported file type reports success=false');
$check(str_contains($bad, '"error_code":"UNSUPPORTED_OPTION"'), 'unsupported file type error_code');

$badUrl = $engine->scrape('not a url');
$check(str_contains($badUrl, '"error_code":"INVALID_REQUEST"'), 'bad url error_code');

$badOpt = $engine->scrape('http://example.invalid/', '{"no_such_option":1}');
$check(str_contains($badOpt, '"error_code":"INVALID_REQUEST"'), 'unknown option is rejected');
$engine->close();

try {
    Engine::create('{"no_such_key":true}');
    $check(false, 'engine create rejects unknown config key');
} catch (InvalidArgumentException $e) {
    $check($e->getMessage() !== '', 'engine create rejects unknown config key');
}

echo "\n{$passed} passed, {$failed} failed\n";
exit($failed === 0 ? 0 : 1);
