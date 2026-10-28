//! 字形文字用的短字符串。
//!
//! 一页几千个字，每个字的文字以前都是一个单独分配的 `String`（字体解码时一次，放上页面时再
//! clone 一次），分配和释放占了 PDF 转换约四成的指令。字形文字几乎都很短（单字、连字 "fi"、
//! ToUnicode 映射出的几个字），22 字节以内的直接存在值里，不分配；更长的才放到堆上。
//! 和 `String` 一样占 24 字节，字形数组照样紧凑；clone 就是一次内存复制。

use std::fmt;
use std::ops::Deref;

const INLINE: usize = 22;

/// 全部 128 个 ASCII 字符：单个 ASCII 字（西文 PDF 里绝大多数字形）从这里切出来，
/// 不用每次都校验 UTF-8（crate 禁止 unsafe，内联字节变回 &str 只能校验或切现成的 str）。
static ASCII_BYTES: [u8; 128] = {
    let mut b = [0u8; 128];
    let mut i = 0;
    while i < 128 {
        b[i] = i as u8;
        i += 1;
    }
    b
};
static ASCII: &str = match std::str::from_utf8(&ASCII_BYTES) {
    Ok(s) => s,
    Err(_) => panic!("ASCII is UTF-8"),
};

#[derive(Clone)]
pub(crate) enum Short {
    Inline { len: u8, buf: [u8; INLINE] },
    Heap(Box<str>),
}

impl Short {
    pub(crate) const fn new() -> Self {
        Short::Inline {
            len: 0,
            buf: [0; INLINE],
        }
    }

    /// 由字符序列拼成：先写进值里的缓冲区，超长了才换成堆上的字符串。
    pub(crate) fn from_chars(chars: impl IntoIterator<Item = char>) -> Self {
        let mut buf = [0; INLINE];
        let mut len = 0;
        let mut chars = chars.into_iter();
        while let Some(c) = chars.next() {
            let n = c.len_utf8();
            if len + n > INLINE {
                let mut s = String::from(std::str::from_utf8(&buf[..len]).unwrap_or_default());
                s.push(c);
                s.extend(chars);
                return Short::Heap(s.into_boxed_str());
            }
            c.encode_utf8(&mut buf[len..]);
            len += n;
        }
        Short::Inline {
            len: len as u8,
            buf,
        }
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Short::Inline { len: 0, .. } => "",
            Short::Inline { len: 1, buf } if buf[0] < 0x80 => {
                let b = usize::from(buf[0]);
                &ASCII[b..b + 1]
            }
            // 只从 &str 构造，字节总是合法的 UTF-8；crate 禁止 unsafe，这里校验一次（几个字节）
            Short::Inline { len, buf } => {
                std::str::from_utf8(&buf[..usize::from(*len)]).unwrap_or_default()
            }
            Short::Heap(s) => s,
        }
    }

    /// 字节长度（不经过 [`Self::as_str`]）。
    pub(crate) fn len(&self) -> usize {
        match self {
            Short::Inline { len, .. } => usize::from(*len),
            Short::Heap(s) => s.len(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for Short {
    fn default() -> Self {
        Short::new()
    }
}

impl From<&str> for Short {
    fn from(s: &str) -> Self {
        if s.len() <= INLINE {
            let mut buf = [0; INLINE];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Short::Inline {
                len: s.len() as u8,
                buf,
            }
        } else {
            Short::Heap(s.into())
        }
    }
}

impl From<String> for Short {
    fn from(s: String) -> Self {
        if s.len() <= INLINE {
            Short::from(s.as_str())
        } else {
            Short::Heap(s.into_boxed_str())
        }
    }
}

impl From<char> for Short {
    fn from(c: char) -> Self {
        Short::from(c.encode_utf8(&mut [0; 4]) as &str)
    }
}

impl Deref for Short {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Short {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for Short {
    fn eq(&self, other: &Short) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Short {}

impl PartialEq<str> for Short {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Short {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl fmt::Debug for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Short {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_stays_inline_and_long_text_goes_to_the_heap() {
        assert_eq!(std::mem::size_of::<Short>(), std::mem::size_of::<String>());
        for s in ["", "a", "fi", "中", "😀", "x".repeat(22).as_str()] {
            let t = Short::from(s);
            assert!(matches!(t, Short::Inline { .. }), "{s:?}");
            assert_eq!(t.as_str(), s);
        }
        let long = "y".repeat(23);
        let t = Short::from(long.clone());
        assert!(matches!(t, Short::Heap(_)));
        assert_eq!(&*t, long.as_str());
        assert_eq!(Short::from('é'), "é");
        for b in 0u8..128 {
            let c = char::from(b);
            let t = Short::from(c);
            assert_eq!(t.as_str().chars().collect::<Vec<_>>(), vec![c]);
            assert_eq!(t.len(), 1);
        }
        assert!(Short::new().is_empty() && Short::from("").as_str().is_empty());
        assert_eq!(Short::from_chars("中文ab".chars()), "中文ab");
        let many = "ä".repeat(30);
        let t = Short::from_chars(many.chars());
        assert!(matches!(t, Short::Heap(_)));
        assert_eq!(&*t, many.as_str());
        assert_eq!(Short::from("ab".to_string()), Short::from("ab"));
    }
}
