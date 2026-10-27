//! Offline components only: no networking, engine selection, or service-level validation.
//! Essence modules are compiled unchanged from their locked checkout (not copied into the product).
#![allow(dead_code)]
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    time::Instant,
};

mod error {
    #[derive(Debug)]
    pub enum ScrapeError {
        InvalidUrl(String),
    }
    impl std::fmt::Display for ScrapeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{self:?}")
        }
    }
    impl std::error::Error for ScrapeError {}
    pub type Result<T> = std::result::Result<T, ScrapeError>;
}
include!(concat!(env!("OUT_DIR"), "/essence.rs"));

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let tool = &args[1];
    let mut out = std::io::BufWriter::new(std::fs::File::create(&args[3])?);
    for line in std::io::BufReader::new(std::fs::File::open(&args[2])?).lines() {
        let row: Value = serde_json::from_str(&line?)?;
        let html = std::fs::read_to_string(row["path"].as_str().ok_or("missing path")?)?;
        let url = row["base_url"]
            .as_str()
            .or(row["url"].as_str())
            .unwrap_or("https://example.com/");
        let only_main = row["only_main"].as_bool().unwrap_or(true);
        let t = Instant::now();
        let result = std::panic::catch_unwind(|| -> Result<Value, String> {
            if tool == "essence" {
                let md = format::markdown::html_to_markdown(&html, url, only_main)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"markdown": md}))
            } else if tool == "pagesift" {
                let opts = pagesift_content::Options {
                    only_main_content: only_main,
                    base_url: url::Url::parse(url).ok(),
                    images: true,
                    links: true,
                    ..Default::default()
                };
                let doc = pagesift_content::process(&html, &opts);
                Ok(json!({"markdown": doc.markdown,
                    "images": doc.images.unwrap_or_default().iter().map(|i| &i.src).collect::<Vec<_>>(),
                    "links": doc.links.unwrap_or_default().iter().map(|i| &i.url).collect::<Vec<_>>()}))
            } else if tool == "pagesift-select" {
                let dom = pagesift_html::Html::parse(&html);
                let selector =
                    pagesift_html::Selector::parse(row["selector"].as_str().unwrap_or("a"))
                        .map_err(|e| e.to_string())?;
                let items: Vec<_> = dom.select(&selector).map(|n| n.text()).collect();
                Ok(json!({"values": items}))
            } else {
                Err(format!("unknown tool: {tool}"))
            }
        });
        let ms = t.elapsed().as_secs_f64() * 1000.;
        let mut record = json!({"id": row["id"], "ms": ms, "status": "ok"});
        match result {
            Ok(Ok(Value::Object(fields))) => record.as_object_mut().unwrap().extend(fields),
            Ok(Err(e)) => {
                record["status"] = json!("error");
                record["error"] = json!(e);
            }
            Err(_) => {
                record["status"] = json!("panic");
            }
            _ => unreachable!(),
        }
        writeln!(out, "{record}")?;
        out.flush()?;
    }
    Ok(())
}
