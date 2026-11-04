// libpagesift 的 Dart 绑定：只用 dart:ffi / dart:convert，没有 pub 依赖。
// 方法返回 Document JSON 字符串，不解析。
// 库的加载顺序：环境变量 PAGESIFT_LIB（库文件完整路径）；然后包内自带的库
// （<包目录>/native/<平台>/，有 linux-x64、linux-arm64、macos-arm64）；最后是系统库名。不联网下载。
library;

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:isolate';
import 'dart:typed_data';

typedef _Str = Pointer<Uint8>;

DynamicLibrary _openLibrary() {
  final explicit = Platform.environment['PAGESIFT_LIB'];
  if (explicit != null && explicit.isNotEmpty) return DynamicLibrary.open(explicit);

  final abi = Abi.current();
  final dir = abi == Abi.linuxX64
      ? 'linux-x64'
      : abi == Abi.linuxArm64
          ? 'linux-arm64'
          : abi == Abi.macosArm64
              ? 'macos-arm64'
              : null;
  final file = Platform.isMacOS ? 'libpagesift.dylib' : 'libpagesift.so';
  if (dir != null) {
    try {
      // 包安装后 package:pagesift/pagesift.dart 指向 <包目录>/lib/pagesift.dart
      final uri = Isolate.resolvePackageUriSync(Uri.parse('package:pagesift/pagesift.dart'));
      if (uri != null) {
        final bundled = File.fromUri(uri.resolve('../native/$dir/$file'));
        if (bundled.existsSync()) return DynamicLibrary.open(bundled.path);
      }
    } on Object {
      // 找不到包目录（例如直接按相对路径 import）就走系统库名
    }
  }
  return DynamicLibrary.open(file);
}

final DynamicLibrary _lib = _openLibrary();

final _version = _lib.lookupFunction<_Str Function(), _Str Function()>('pagesift_version');
final _stringFree =
    _lib.lookupFunction<Void Function(_Str), void Function(_Str)>('pagesift_string_free');
final _engineNew = _lib.lookupFunction<Pointer<Void> Function(_Str, Pointer<_Str>),
    Pointer<Void> Function(_Str, Pointer<_Str>)>('pagesift_engine_new');
final _engineFree = _lib
    .lookupFunction<Void Function(Pointer<Void>), void Function(Pointer<Void>)>('pagesift_engine_free');
final _convert = _lib.lookupFunction<_Str Function(_Str, _Str), _Str Function(_Str, _Str)>('pagesift_convert');
final _parse = _lib.lookupFunction<
    _Str Function(Pointer<Void>, _Str, IntPtr, _Str, _Str),
    _Str Function(Pointer<Void>, _Str, int, _Str, _Str)>('pagesift_parse');
final _scrape = _lib.lookupFunction<_Str Function(Pointer<Void>, _Str, _Str),
    _Str Function(Pointer<Void>, _Str, _Str)>('pagesift_scrape');

// Dart 字符串 → 以 NUL 结尾的 UTF-8（自己 malloc/free，不依赖 package:ffi）；null 为 NULL。
final _malloc = DynamicLibrary.process().lookupFunction<_Str Function(IntPtr), _Str Function(int)>('malloc');
final _free = DynamicLibrary.process().lookupFunction<Void Function(_Str), void Function(_Str)>('free');

_Str _cbytes(List<int> bytes) {
  final p = _malloc(bytes.length + 1);
  p.asTypedList(bytes.length + 1)
    ..setAll(0, bytes)
    ..[bytes.length] = 0;
  return p;
}

_Str _cstr(String? s) => s == null ? nullptr.cast() : _cbytes(utf8.encode(s));

void _release(_Str p) {
  if (p != nullptr) _free(p);
}

String _take(_Str p) {
  if (p == nullptr) throw StateError('pagesift returned NULL');
  var n = 0;
  while (p[n] != 0) {
    n++;
  }
  final s = utf8.decode(Uint8List.fromList(p.asTypedList(n)));
  _stringFree(p);
  return s;
}

String version() {
  final p = _version();
  var n = 0;
  while (p[n] != 0) {
    n++;
  }
  return utf8.decode(p.asTypedList(n));
}

String convert(String html, [String? optionsJson]) {
  final h = _cstr(html), o = _cstr(optionsJson);
  try {
    return _take(_convert(h, o));
  } finally {
    _release(h);
    _release(o);
  }
}

class Engine {
  Pointer<Void>? _h;
  Engine._(this._h);

  /// 建引擎；configJson 为 null 用默认配置。配置不合法抛 ArgumentError。
  factory Engine([String? configJson]) {
    final cfg = _cstr(configJson);
    final err = _malloc(8).cast<_Str>();
    err.value = nullptr.cast();
    try {
      final e = _engineNew(cfg, err);
      if (e == nullptr) {
        final msg = err.value == nullptr ? 'engine_new failed' : _take(err.value);
        throw ArgumentError(msg);
      }
      return Engine._(e);
    } finally {
      _release(cfg);
      _free(err.cast());
    }
  }

  Pointer<Void> get _live => _h ?? (throw StateError('engine is closed'));

  String parse(List<int> data, String filename, [String? optionsJson]) {
    final h = _live;
    final d = _cbytes(data), f = _cstr(filename), o = _cstr(optionsJson);
    try {
      return _take(_parse(h, data.isEmpty ? nullptr.cast() : d, data.length, f, o));
    } finally {
      _release(d);
      _release(f);
      _release(o);
    }
  }

  String scrape(String url, [String? optionsJson]) {
    final h = _live;
    final u = _cstr(url), o = _cstr(optionsJson);
    try {
      return _take(_scrape(h, u, o));
    } finally {
      _release(u);
      _release(o);
    }
  }

  void close() {
    final h = _h;
    if (h != null) {
      _engineFree(h);
      _h = null;
    }
  }
}
