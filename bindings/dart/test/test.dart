// Dart 绑定测试：每项检查打印结果，有失败就非零退出。不联网。
import 'dart:io';

import '../pagesift.dart' as pagesift;

int passed = 0, failed = 0;
void check(bool ok, String what) {
  if (ok) {
    passed++;
    print('  ok   $what');
  } else {
    failed++;
    print('  FAIL $what');
  }
}

void main() {
  final v = pagesift.version();
  print('libpagesift $v');
  check(RegExp(r'^\d+\.\d+\.\d+').hasMatch(v), 'version looks like x.y.z');

  final md = pagesift.convert('<h1>Hi</h1><p>World</p>');
  check(md.contains('"success":true'), 'convert succeeds');
  check(md.contains('# Hi'), 'convert keeps the heading');

  final e = pagesift.Engine();
  final doc = e.parse('name,score\nalice,1\nbob,2\n'.codeUnits, 'scores.csv');
  check(doc.contains('"success":true'), 'parse csv succeeds');
  check(doc.contains('| alice | 1 |'), 'parse csv renders a table row');
  final bad = e.parse([0, 1, 2, 3], 'blob.xyz');
  check(bad.contains('"success":false'), 'unsupported file type reports success=false');
  check(bad.contains('"error_code":"UNSUPPORTED_OPTION"'), 'unsupported file type error_code');
  check(e.scrape('not a url').contains('"error_code":"INVALID_REQUEST"'), 'bad url error_code');
  e.close();
  var raised = false;
  try {
    e.scrape('http://example.invalid/');
  } on StateError {
    raised = true;
  }
  check(raised, 'closed engine raises');

  var rejected = false;
  try {
    pagesift.Engine('{"no_such_key":true}');
  } on ArgumentError {
    rejected = true;
  }
  check(rejected, 'engine create rejects unknown config key');

  print('\n$passed passed, $failed failed');
  exit(failed == 0 ? 0 : 1);
}
