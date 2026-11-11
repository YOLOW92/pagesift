//! PageSift 的 Base64 编解码层，直接重导出 [`yolow64`]。
//!
//! ```
//! let s = pagesift_base64::STANDARD.encode_to_string(b"hello").unwrap();
//! assert_eq!(s, "aGVsbG8=");
//! assert_eq!(pagesift_base64::STANDARD.decode_to_vec(s.as_bytes()).unwrap(), b"hello");
//! ```

#![forbid(unsafe_code)]

pub use yolow64::*;
