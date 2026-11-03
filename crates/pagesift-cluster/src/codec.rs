//! 帧与日志共用的二进制编码：每种字段类型实现 [`Codec`]，消息与日志记录用 [`codec_enum!`]
//! 从一张"标签 → 变体 {字段: 类型}"的表同时生成枚举和编解码。
//!
//! 底层是 `pagesift_stat` 的 `Writer`/`Reader`：小端定长整数和浮点、`u32` 长度前缀的序列。
//! `Option<T>` 是一个字节的有无标记加值；`Result<T, Error>` 是一个字节的成败标记加值或错误；
//! `Vec<T>` 是 `u32` 个数加各元素，其中 `Vec<u8>` 走字节串快路径（格式相同：长度 + 原样字节）。

use std::time::Duration;

use pagesift_stat::{DecodeError, Reader, Writer};

use crate::clock::ms;
use crate::hrw::HostMap;
use crate::types::{
    Deadline, Error, Fence, Renewal, Resource, Resources, Scope, SiteKey, SiteState, StatKind,
    StatsDelta, Task, TaskId, WorkerId,
};

pub(crate) trait Codec: Sized {
    fn put(&self, w: &mut Writer);
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError>;

    /// `Vec<Self>` 的编码；`u8` 改写成整段字节串。
    fn put_vec(v: &[Self], w: &mut Writer) {
        w.u32(u32::try_from(v.len()).expect("sequence longer than u32::MAX"));
        for x in v {
            x.put(w);
        }
    }

    fn get_vec(r: &mut Reader<'_>) -> Result<Vec<Self>, DecodeError> {
        let n = r.u32()?;
        (0..n).map(|_| Self::get(r)).collect()
    }
}

impl Codec for u8 {
    fn put(&self, w: &mut Writer) {
        w.u8(*self);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        r.u8()
    }
    fn put_vec(v: &[u8], w: &mut Writer) {
        w.bytes(v);
    }
    fn get_vec(r: &mut Reader<'_>) -> Result<Vec<u8>, DecodeError> {
        Ok(r.bytes()?.to_vec())
    }
}

/// 只是包一层 `Writer`/`Reader` 方法的类型。
macro_rules! codec_via {
    ($($t:ty => $put:ident, $get:ident;)*) => {$(
        impl Codec for $t {
            fn put(&self, w: &mut Writer) {
                w.$put(*self);
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
                r.$get()
            }
        }
    )*};
}

codec_via! {
    u32 => u32, u32;
    u64 => u64, u64;
    f64 => f64, f64;
}

/// 单字段新类型。
macro_rules! codec_newtype {
    ($($t:ident($inner:ty);)*) => {$(
        impl Codec for $t {
            fn put(&self, w: &mut Writer) {
                self.0.put(w);
            }
            fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
                Ok(Self(<$inner>::get(r)?))
            }
        }
    )*};
}

codec_newtype! {
    TaskId(u64);
    SiteKey(u64);
    StatKind(u32);
    Deadline(u64);
}

/// 结构体：字段按列出的顺序编码。
macro_rules! codec_struct {
    ($($t:ident { $($f:ident),* $(,)? })*) => {$(
        impl $crate::codec::Codec for $t {
            fn put(&self, w: &mut pagesift_stat::Writer) {
                $($crate::codec::Codec::put(&self.$f, w);)*
            }
            fn get(
                r: &mut pagesift_stat::Reader<'_>,
            ) -> Result<Self, pagesift_stat::DecodeError> {
                Ok(Self { $($f: $crate::codec::Codec::get(r)?),* })
            }
        }
    )*};
}

codec_struct! {
    Fence { term, epoch }
    Task { site, class, timeout, need, body }
    StatsDelta { kind, seq, table }
    SiteState { site, wait, state }
}

/// 一张"标签 → 变体 {字段: 类型}"的表，生成枚举、`tag()`、`put_body()`、`get_body()`。
/// 每个变体写成结构体形式（无字段写 `{}`）；标签是格式的一部分，改动要升格式版本。
macro_rules! codec_enum {
    (
        $(#[$em:meta])* $vis:vis enum $name:ident {
            $( $(#[$m:meta])* $tag:literal => $var:ident { $($f:ident : $t:ty),* $(,)? } ),* $(,)?
        }
    ) => {
        $(#[$em])* $vis enum $name {
            $( $(#[$m])* $var { $($f: $t),* } ),*
        }

        impl $name {
            pub(crate) fn tag(&self) -> u8 {
                match self { $( Self::$var { .. } => $tag ),* }
            }

            pub(crate) fn put_body(&self, w: &mut pagesift_stat::Writer) {
                match self {
                    $( Self::$var { $($f),* } => { $( $crate::codec::Codec::put($f, w); )* } )*
                }
            }

            pub(crate) fn get_body(
                tag: u8,
                r: &mut pagesift_stat::Reader<'_>,
            ) -> Result<Self, pagesift_stat::DecodeError> {
                Ok(match tag {
                    $( $tag => Self::$var { $($f: <$t as $crate::codec::Codec>::get(r)?),* }, )*
                    _ => return Err(pagesift_stat::DecodeError::Invalid(stringify!($name tag))),
                })
            }
        }
    };
}
pub(crate) use codec_enum;
pub(crate) use codec_struct;

impl Codec for bool {
    fn put(&self, w: &mut Writer) {
        w.u8(u8::from(*self));
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(r.u8()? != 0)
    }
}

impl Codec for Duration {
    fn put(&self, w: &mut Writer) {
        w.u64(ms(*self));
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(Duration::from_millis(r.u64()?))
    }
}

impl Codec for String {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(String::from_utf8_lossy(r.bytes()?).into_owned())
    }
}

impl Codec for std::sync::Arc<str> {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let s = std::str::from_utf8(r.bytes()?).map_err(|_| DecodeError::Invalid("utf-8"))?;
        Ok(s.into())
    }
}

impl Codec for WorkerId {
    fn put(&self, w: &mut Writer) {
        w.bytes(self.as_str().as_bytes());
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let s = std::str::from_utf8(r.bytes()?).map_err(|_| DecodeError::Invalid("worker id"))?;
        Ok(WorkerId::new(s))
    }
}

impl Codec for Resources {
    fn put(&self, w: &mut Writer) {
        for k in Resource::ALL {
            w.f64(self.get(k));
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let mut x = Resources::default();
        for k in Resource::ALL {
            x = x.with(k, r.f64()?);
        }
        Ok(x)
    }
}

impl Codec for HostMap {
    fn put(&self, w: &mut Writer) {
        self.encode(w);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        HostMap::decode(r)
    }
}

impl Codec for Scope {
    fn put(&self, w: &mut Writer) {
        let (tag, id) = match *self {
            Scope::Global => (0, 0),
            Scope::Site(h) => (1, h),
            // [协议] 与 pagesift_stat::table 的作用域标签相同：0 全局、1 站点、2 路径族
            Scope::Family(h) => (2, h),
        };
        w.u8(tag);
        w.u64(id);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match (r.u8()?, r.u64()?) {
            (0, _) => Scope::Global,
            (1, h) => Scope::Site(h),
            (2, h) => Scope::Family(h),
            _ => return Err(DecodeError::Invalid("scope tag")),
        })
    }
}

impl Codec for Renewal {
    fn put(&self, w: &mut Writer) {
        match self {
            Renewal::Cancel => w.u8(0),
            Renewal::Extended { expires, ttl } => {
                w.u8(1);
                expires.put(w);
                ttl.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match r.u8()? {
            0 => Renewal::Cancel,
            _ => Renewal::Extended {
                expires: Codec::get(r)?,
                ttl: Codec::get(r)?,
            },
        })
    }
}

impl<T: Codec> Codec for Vec<T> {
    fn put(&self, w: &mut Writer) {
        T::put_vec(self, w);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        T::get_vec(r)
    }
}

impl<T: Codec> Codec for Option<T> {
    fn put(&self, w: &mut Writer) {
        match self {
            None => w.u8(0),
            Some(x) => {
                w.u8(1);
                x.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match r.u8()? {
            0 => None,
            _ => Some(T::get(r)?),
        })
    }
}

impl<A: Codec, B: Codec> Codec for (A, B) {
    fn put(&self, w: &mut Writer) {
        self.0.put(w);
        self.1.put(w);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok((A::get(r)?, B::get(r)?))
    }
}

impl<T: Codec> Codec for Result<T, Error> {
    fn put(&self, w: &mut Writer) {
        match self {
            Ok(v) => {
                w.u8(0);
                v.put(w);
            }
            Err(e) => {
                w.u8(1);
                e.put(w);
            }
        }
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(match r.u8()? {
            0 => Ok(T::get(r)?),
            _ => Err(Error::get(r)?),
        })
    }
}

impl Codec for () {
    fn put(&self, _: &mut Writer) {}
    fn get(_: &mut Reader<'_>) -> Result<Self, DecodeError> {
        Ok(())
    }
}

codec_enum! {
    /// 线上的错误（与 [`Error`] 一一对应）。
    enum WireError {
        1 => Fenced { current: Fence }, // [协议] 错误标签
        2 => Cancelled {},              // [协议] 同上
        3 => UnknownTask {},            // [协议] 同上
        4 => Rejected { m: String },    // [协议] 同上
        5 => Transport { m: String },   // [协议] 同上
        6 => Store { m: String },       // [协议] 同上
        7 => Closed {},                 // [协议] 同上
    }
}

impl Codec for Error {
    fn put(&self, w: &mut Writer) {
        let e = match self.clone() {
            Error::Fenced { current } => WireError::Fenced { current },
            Error::Cancelled => WireError::Cancelled {},
            Error::UnknownTask => WireError::UnknownTask {},
            Error::Rejected(m) => WireError::Rejected { m },
            Error::Transport(m) => WireError::Transport { m },
            Error::Store(m) => WireError::Store { m },
            Error::Closed => WireError::Closed {},
        };
        w.u8(e.tag());
        e.put_body(w);
    }
    fn get(r: &mut Reader<'_>) -> Result<Self, DecodeError> {
        let tag = r.u8()?;
        Ok(match WireError::get_body(tag, r)? {
            WireError::Fenced { current } => Error::Fenced { current },
            WireError::Cancelled {} => Error::Cancelled,
            WireError::UnknownTask {} => Error::UnknownTask,
            WireError::Rejected { m } => Error::Rejected(m),
            WireError::Transport { m } => Error::Transport(m),
            WireError::Store { m } => Error::Store(m),
            WireError::Closed {} => Error::Closed,
        })
    }
}
