use std::env;
use std::fs;
use pagesift_extract::extract_structured;

fn main() {
    let path = env::args().nth(1).expect("usage: test_structured <html_file>");
    let html = fs::read_to_string(&path).expect("read file");
    let res = extract_structured(&html);
    println!("{}", serde_json::to_string_pretty(&res).unwrap());
}
