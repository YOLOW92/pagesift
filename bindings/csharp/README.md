# pagesift for C#

A P/Invoke binding of [pagesift](https://github.com/YOLOW92/pagesift) for .NET 10, with no NuGet dependencies.

## Install

```sh
dotnet add package Pagesift
```

The package includes the library for Linux x86_64, Linux aarch64 and macOS arm64. There is no Windows library.

## Usage

```csharp
using PagesiftLib;

string md = Pagesift.Convert("<h1>Hi</h1>");
using var engine = Pagesift.Engine.Create();
string doc = engine.Parse(csvBytes, "scores.csv");
```

Functions return the Document as a JSON string and do not parse it. A bad engine configuration throws `ArgumentException`.

## Finding the library

1. The environment variable `PAGESIFT_LIB` (full path to the library file).
2. The library inside the package.
3. The system library name (`libpagesift.so`, `libpagesift.dylib`).

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/csharp/run_tests.sh
```

The tests do not use the network.

## License

AGPL-3.0-only, with commercial licensing available. See the [main repository](https://github.com/YOLOW92/pagesift).
