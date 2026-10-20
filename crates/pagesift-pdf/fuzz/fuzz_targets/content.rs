//! 内容流词法器。
#![no_main]

use libfuzzer_sys::fuzz_target;
use pagesift_pdf::fuzzing;

fuzz_target!(|data: &[u8]| {
    fuzzing::lex(data);
});
