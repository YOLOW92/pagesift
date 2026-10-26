//! extractous：Office / 电子书 / PDF → 纯文本（extractous 只出纯文本，不出 Markdown），默认 Extractor。
use extractous::Extractor;

fn main() {
    let extractor = Extractor::new();
    mix_proto::serve("extractous", "extractous 0.3.0", |input| {
        extractor
            .extract_file_to_string(input)
            .map(|(text, _meta)| text)
            .map_err(|e| e.to_string())
    });
}
