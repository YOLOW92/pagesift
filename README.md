# pagesift

A single-binary web scraping engine written in Rust. It turns web pages into clean Markdown for LLMs and agents, speaks the Firecrawl v2 API, and plays by the rules: it respects robots.txt and does not bypass anti-bot protections.

> **Status: early development.** Nothing here is ready for use yet. The design is in [pagesift-PRD.md](pagesift-PRD.md) (Chinese).

## What works today

`pagesift-content` converts HTML to Markdown, keeping only the main content:

```bash
cargo run -p pagesift -- convert page.html --base-url https://example.com/post
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
