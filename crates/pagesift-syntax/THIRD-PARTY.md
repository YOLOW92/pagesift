# 第三方来源

本 crate 的设计参照 tree-sitter 运行时（MIT，Copyright (c) 2014 Max Brunsfeld），
解析表格式和 `parser.c` 宏的理解来自其公开源码；错误恢复、版本合并与裁剪的做法按其算法移植。
后续收进来的每个语法在此登记：来源仓库、版本、许可证、版权声明。只收宽松许可（MIT/Apache-2.0/BSD）的语法。

## 运行时参照

- tree-sitter（https://github.com/tree-sitter/tree-sitter），提交 9632b56（2026-11-02），MIT。
  Copyright (c) 2014 Max Brunsfeld

## 语法

语法表由 `ts-convert` 从各仓库生成的 `src/parser.c` 转换而来；仓库里只放 JSON 的表和 corpus 做测试，
其余语言的表放在仓库之外，不随代码分发。以下均为 MIT 许可，版权声明如下。

| 语法 | 仓库 | 版本 / 提交 | 版权声明 |
| --- | --- | --- | --- |
| JSON | tree-sitter/tree-sitter-json | 0.24.8 / 254c42a | Copyright (c) 2014 Max Brunsfeld |
| C | tree-sitter/tree-sitter-c | 0.24.2 / b780e47 | Copyright (c) 2014 Max Brunsfeld |
| Go | tree-sitter/tree-sitter-go | 0.25.0 / 2346a3a | Copyright (c) 2014 Max Brunsfeld |
| Java | tree-sitter/tree-sitter-java | 0.23.5 / e10607b | Copyright (c) 2017 Ayman Nadeem |
| Python | tree-sitter/tree-sitter-python | 0.25.0 / 26855ea | Copyright (c) 2016 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| JavaScript | tree-sitter/tree-sitter-javascript | 0.25.0 / 58404d8 | Copyright (c) 2014 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| TypeScript、TSX | tree-sitter/tree-sitter-typescript | 0.23.2 / 75b3874 | Copyright (c) 2017 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| Rust | tree-sitter/tree-sitter-rust | 0.24.2 / 77a3747 | Copyright (c) 2017 Maxim Sokolov；外部扫描器移植自该仓库 src/scanner.c |
| C++ | tree-sitter/tree-sitter-cpp | 0.23.4 / c009222 | Copyright (c) 2014 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| Lua | MunifTanjim/tree-sitter-lua | 0.3.1-dev / 4fbec84 | Copyright (c) 2021 Munif Tanjim；外部扫描器移植自该仓库 src/scanner.c |
| Bash | tree-sitter/tree-sitter-bash | 0.25.1 / a06c2e4 | Copyright (c) 2017 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| Ruby | tree-sitter/tree-sitter-ruby | 0.23.1 / ad907a6 | Copyright (c) 2016 Rob Rix；外部扫描器移植自该仓库 src/scanner.c |
| Kotlin | fwcd/tree-sitter-kotlin | 0.4.0 / 5fd3624 | Copyright (c) 2019 fwcd；外部扫描器移植自该仓库 src/scanner.c |
| CSS | tree-sitter/tree-sitter-css | 0.25.0 / dda5cfc | Copyright (c) 2018 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c |
| HTML | tree-sitter/tree-sitter-html | 0.23.2 / 73a3947 | Copyright (c) 2014 Max Brunsfeld；外部扫描器移植自该仓库 src/scanner.c 和 src/tag.h |
| Objective-C | tree-sitter-grammars/tree-sitter-objc | 3.0.2 / 181a81b | Copyright (c) 2023 Amaan Qureshi |
| Dart | UserNobody14/tree-sitter-dart | 1.0.0 / 0038280 | Copyright (c) 2020-2023 UserNobody14 and others；外部扫描器移植自该仓库 src/scanner.c |
| Julia | tree-sitter/tree-sitter-julia | 0.25.0 / e0f9dcd | Copyright (c) 2018 Max Brunsfeld, GitHub；外部扫描器移植自该仓库 src/scanner.c |
| Nix | nix-community/tree-sitter-nix | 0.3.0 / e49c2f3 | Copyright (c) 2019 Charles Strahan；外部扫描器移植自该仓库 src/scanner.c |
| Dockerfile | camdencheek/tree-sitter-dockerfile | 0.2.0 / 971acdd | Copyright (c) 2021 Camden Cheek；外部扫描器移植自该仓库 src/scanner.c |
| OCaml | tree-sitter/tree-sitter-ocaml | 0.26.0 / 3b2e14e | Copyright (c) 2020 Max Brunsfeld and Pieter Goetschalckx；外部扫描器移植自该仓库 common/scanner.h |
| Elixir | elixir-lang/tree-sitter-elixir | 0.3.5 / cb32ccb | Apache License 2.0；外部扫描器移植自该仓库 src/scanner.c |
| R | r-lib/tree-sitter-r | 1.3.0 / 58a2279 | Copyright (c) 2025 tree-sitter-r authors；外部扫描器移植自该仓库 src/scanner.c |
| Svelte | tree-sitter-grammars/tree-sitter-svelte | 1.0.2 / ae5199d | Copyright (c) 2024 Amaan Qureshi；标签栈扫描与 HTML 共用，花括号文本部分移植自该仓库 src/scanner.c |
| Solidity | JoranHonig/tree-sitter-solidity | 1.2.13 / 048fe68 | Copyright (c) 2020 Joran Honig |
| Groovy | murtaza64/tree-sitter-groovy | 0.0.1 / 2a6ddd5 | Copyright (c) 2024 Murtaza Javaid |
| Make | tree-sitter-grammars/tree-sitter-make | 1.1.1 / 70613f3 | Copyright (c) 2021 Alexandre A. Muller |
| Scheme | 6cdh/tree-sitter-scheme | 0.25.0 / 45c7246 | Copyright (c) 2022 6cdh |
| Common Lisp | tree-sitter-grammars/tree-sitter-commonlisp | 0.4.1 / 3232350 | Copyright (c) 2021 Stephan Seitz |
| Pascal | Isopod/tree-sitter-pascal | 0.10.2 / 042119e | Copyright (c) 2018 Benjamin Gray |
| Gleam | gleam-lang/tree-sitter-gleam | 1.0.0 / fa6d0d9 | Apache License 2.0；外部扫描器移植自该仓库 src/scanner.c |
| PowerShell | airbus-cert/tree-sitter-powershell | 0.26.6 / 65ea691 | Copyright (c) 2023 Airbus CERT；外部扫描器移植自该仓库 src/scanner.c（Copyright (c) Microsoft Corporation，MIT） |
| CMake | uyha/tree-sitter-cmake | 0.7.6 / a3ec2b2 | Copyright (c) 2025 Uy Ha；外部扫描器移植自该仓库 src/scanner.c |
| Erlang | WhatsApp/tree-sitter-erlang | 0.0.0 / 62c82e6 | Apache License 2.0，Copyright (c) Meta Platforms, Inc. and affiliates；外部扫描器移植自该仓库 src/scanner.c |
| HCL | tree-sitter-grammars/tree-sitter-hcl | 1.2.0 / 64ad627 | Apache License 2.0；外部扫描器移植自该仓库 src/scanner.c |
| Vue | tree-sitter-grammars/tree-sitter-vue | 0.1.0 / ce8011a | Copyright (c) 2024 Amaan Qureshi；标签栈扫描与 HTML 共用，文本片段部分移植自该仓库 src/scanner.c |
| D | gdamore/tree-sitter-d | 0.8.2 / 64f2793 | Copyright 2024 Garrett D'Amore；外部扫描器移植自该仓库 src/scanner.c |
| TOML | tree-sitter-grammars/tree-sitter-toml | 0.7.0 / 64b5683 | Copyright (c) Ika；外部扫描器移植自该仓库 src/scanner.c |
| PHP、PHP-only | tree-sitter/tree-sitter-php | 0.25.1 / 58d5643 | Copyright (c) 2017 Josh Vera, GitHub；外部扫描器移植自该仓库 src/scanner.c |
| C# | tree-sitter/tree-sitter-c-sharp | 0.23.5 / 8c0abe0 | Copyright (c) 2014-2023 Max Brunsfeld, Damien Guard, Amaan Qureshi, and contributors；外部扫描器移植自该仓库 src/scanner.c |

`tests/data/json.pslg` 是 tree-sitter-json 的转换结果，`tests/data/json-corpus.txt` 是它的 `test/corpus/main.txt` 原文。

MIT 许可全文：

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
> documentation files (the "Software"), to deal in the Software without restriction, including without limitation
> the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and
> to permit persons to whom the Software is furnished to do so, subject to the following conditions: The above
> copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED.
