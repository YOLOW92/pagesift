# pagesift for Perl

A binding of [pagesift](https://github.com/YOLOW92/pagesift) using [FFI::Platypus](https://metacpan.org/pod/FFI::Platypus). It is not published to CPAN; use it from this repository.

## Usage

```perl
use Pagesift;
my $md = Pagesift::convert("<h1>Hi</h1>");
my $e = Pagesift::engine();
my $doc = $e->parse($csv_bytes, "scores.csv");
$e->close;
```

Functions return the Document as a JSON string and do not parse it.

## Finding the library

Set `PAGESIFT_LIB` to the full path of the library file. Without it, the module looks for `target/f/quick/libpagesift.so` in this repository.

## Tests

From the repository root:

```sh
CARGO_TARGET_DIR=target/f cargo build --profile quick -p pagesift-ffi
./bindings/perl/run_tests.sh
```

The tests do not use the network.

## License

Apache-2.0. See the [main repository](https://github.com/YOLOW92/pagesift).
