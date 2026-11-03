//! 解码：serde `Deserializer`。所有入口都按自描述读（`deserialize_any`），坏数据只报错、不 panic。

use serde::de::{self, DeserializeSeed, Visitor};

use super::{Error, tag};

/// 从字节串解码。末尾有多余字节也算错（防止拿错格式的数据当成功）。
pub fn from_slice<'de, T: de::Deserialize<'de>>(bytes: &'de [u8]) -> Result<T, Error> {
    let mut d = De { input: bytes };
    let v = T::deserialize(&mut d)?;
    if d.input.is_empty() {
        Ok(v)
    } else {
        Err(Error("trailing bytes".into()))
    }
}

struct De<'de> {
    input: &'de [u8],
}

fn eof() -> Error {
    Error("unexpected end of input".into())
}

impl<'de> De<'de> {
    fn peek(&self) -> Result<u8, Error> {
        self.input.first().copied().ok_or_else(eof)
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let (&b, rest) = self.input.split_first().ok_or_else(eof)?;
        self.input = rest;
        Ok(b)
    }

    fn uint(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        // [协议] LEB128：最多 10 个字节（64 位 / 每字节 7 位，向上取整）
        for shift in (0..64).step_by(7) {
            let b = self.byte()?;
            v |= u64::from(b & 0x7f) << shift; // [协议] LEB128 的 7 位数据掩码
            if b & 0x80 == 0 {
                // [协议] LEB128 续位
                return Ok(v);
            }
        }
        Err(Error("varint too long".into()))
    }

    fn take(&mut self, n: u64) -> Result<&'de [u8], Error> {
        let n = usize::try_from(n).map_err(|_| eof())?;
        if n > self.input.len() {
            return Err(eof());
        }
        let (head, rest) = self.input.split_at(n);
        self.input = rest;
        Ok(head)
    }

    fn str(&mut self) -> Result<&'de str, Error> {
        let len = self.uint()?;
        std::str::from_utf8(self.take(len)?).map_err(|e| Error(e.to_string()))
    }

    fn end(&mut self) -> Result<(), Error> {
        match self.byte()? {
            tag::END => Ok(()),
            _ => Err(Error("expected end marker".into())),
        }
    }
}

impl<'de> de::Deserializer<'de> for &mut De<'de> {
    type Error = Error;

    /// 内部二进制格式：类型据此选紧凑表示（例如附件连内容一起编码，JSON 里只有元数据）。
    fn is_human_readable(&self) -> bool {
        false
    }

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.byte()? {
            tag::NULL => visitor.visit_unit(),
            tag::FALSE => visitor.visit_bool(false),
            tag::TRUE => visitor.visit_bool(true),
            tag::UINT => visitor.visit_u64(self.uint()?),
            tag::INT => {
                let z = self.uint()?;
                visitor.visit_i64(((z >> 1) as i64) ^ -((z & 1) as i64))
            }
            tag::FLOAT => {
                let b = self.take(8)?; // [协议] f64 占 8 字节
                visitor.visit_f64(f64::from_le_bytes(b.try_into().map_err(|_| eof())?))
            }
            tag::STR => visitor.visit_borrowed_str(self.str()?),
            tag::BYTES => {
                let len = self.uint()?;
                visitor.visit_borrowed_bytes(self.take(len)?)
            }
            tag::SEQ => {
                let v = visitor.visit_seq(Items { de: self })?;
                self.end()?;
                Ok(v)
            }
            tag::MAP => {
                let v = visitor.visit_map(Items { de: self })?;
                self.end()?;
                Ok(v)
            }
            t => Err(Error(format!("unknown tag {t}"))),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if self.peek()? == tag::NULL {
            self.input = &self.input[1..];
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.byte()? {
            tag::STR => visitor.visit_enum(de::value::BorrowedStrDeserializer::<Error>::new(
                self.str()?,
            )),
            tag::MAP => {
                if self.byte()? != tag::STR {
                    return Err(Error("expected variant name".into()));
                }
                let variant = self.str()?;
                let v = visitor.visit_enum(Variant { de: self, variant })?;
                self.end()?;
                Ok(v)
            }
            _ => Err(Error("expected enum".into())),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

/// seq 和 map 的元素：读到 `END` 为止（`END` 本身由调用方吃掉）。
struct Items<'a, 'de> {
    de: &'a mut De<'de>,
}

impl<'de> de::SeqAccess<'de> for Items<'_, 'de> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        if self.de.peek()? == tag::END {
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
}

impl<'de> de::MapAccess<'de> for Items<'_, 'de> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        if self.de.peek()? == tag::END {
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        seed.deserialize(&mut *self.de)
    }
}

/// `{变体名: 值}` 形式的枚举。
struct Variant<'a, 'de> {
    de: &'a mut De<'de>,
    variant: &'de str,
}

impl<'de, 'a> de::EnumAccess<'de> for Variant<'a, 'de> {
    type Error = Error;
    type Variant = Self;
    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let name = de::value::BorrowedStrDeserializer::<Error>::new(self.variant);
        Ok((seed.deserialize(name)?, self))
    }
}

impl<'de> de::VariantAccess<'de> for Variant<'_, 'de> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        de::Deserialize::deserialize(&mut *self.de)
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(&mut *self.de)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_any(&mut *self.de, visitor)
    }
}
