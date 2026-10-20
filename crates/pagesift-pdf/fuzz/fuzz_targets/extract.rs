//! 整个文件当 PDF 解析。
#![no_main]

use std::time::Duration;

use libfuzzer_sys::fuzz_target;
use pagesift_pdf::{Options, fuzzing};

fuzz_target!(|data: &[u8]| {
    let opts = Options {
        timeout: Some(Duration::from_secs(2)),
        ..Options::default()
    };
    let _ = fuzzing::extract_unguarded(data, &opts);
});
