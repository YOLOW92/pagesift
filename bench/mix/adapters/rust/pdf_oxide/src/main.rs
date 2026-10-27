//! pdf_oxide：PDF → Markdown。默认特性，ConversionOptions::default()，整份文档一次转换。
use pdf_oxide::converters::ConversionOptions;
use pdf_oxide::PdfDocument;

fn main() {
    mix_proto::serve("pdf_oxide", "pdf_oxide 0.3.78", |input| {
        let doc = PdfDocument::open(input).map_err(|e| e.to_string())?;
        doc.to_markdown_all(&ConversionOptions::default())
            .map_err(|e| e.to_string())
    });
}
