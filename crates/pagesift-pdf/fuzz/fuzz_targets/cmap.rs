//! CMap 解析和查表，前 4 个字节当作要查的字符码。
#![no_main]

use libfuzzer_sys::fuzz_target;
use pagesift_pdf::fuzzing;

fuzz_target!(|data: &[u8]| {
    let (probe, cmap) = data.split_at(data.len().min(4));
    fuzzing::cmap(cmap, probe);
});
