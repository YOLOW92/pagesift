# HTML → Markdown fidelity

There is no standard dataset for how faithfully a tool turns HTML into Markdown, so this benchmark
generates one: `fidelity.py` builds random documents from a fixed seed (headings, paragraphs,
strong/emphasis/strikethrough, inline code, links, images, line breaks, nested ordered and bullet
lists, tables, fenced code with a language, block quotes, rules, inline and display formulas, and
text containing Markdown characters that must stay literal: `*not bold*`, `_not em_`, `1. not a
list`, `# not a heading`, `<tag>`, `x | y`, …). It renders them the way real sites do: wrapper
`div`s and `span`s, indented source, entities, `thead`/`tbody`, syntax-highlighted code (one span
per token), KaTeX and MathJax 2 output, and raw TeX left in the text for MathJax to render in the
browser (what an HTTP fetch of a MathJax site returns).

Every tool converts the HTML; each output is parsed back with the same GFM parser (markdown-it-py,
`gfm-like`) and compared with the document's structure, category by category (precision, recall and
F1, all higher is better; the overall score is the mean F1). Formulas are matched by their TeX in
`$…$`, `$$…$$`, `\(…\)` or `\[…\]`. Paired backticks in prose are not used as "literal text": on the
web they are almost always Markdown that the author typed and the site did not render, so turning
them into code is as reasonable as keeping them literal.

The scorer is calibrated with a reference conversion written directly from each document's
structure (`<dir>/ref`): it scores 1.000 in every category.

```bash
benchmarks/markdown/run.sh          # seeds 1 2 3, 200 documents each; results in benchmarks/markdown/out/seed*/
```

Tools: pagesift (whole page, `--full`), markdownify, html2text (GPL, evaluation only), MarkItDown,
Crawl4AI's `DefaultMarkdownGenerator` (`raw_markdown`), Turndown (ATX headings, fenced code; the GFM
plugin is not installed, so it has no tables or strikethrough), Defuddle (`markdown: true`).
