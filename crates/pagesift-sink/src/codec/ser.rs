//! 编码：serde `Serializer`。

use serde::ser::{self, Serialize};

use super::{Error, tag};

/// 编码成新的字节串。
pub fn to_vec<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    to_writer(&mut out, value)?;
    Ok(out)
}

/// 追加到已有缓冲的末尾。
pub fn to_writer<T: Serialize + ?Sized>(out: &mut Vec<u8>, value: &T) -> Result<(), Error> {
    value.serialize(&mut Ser { out })
}

pub(crate) fn put_uint(out: &mut Vec<u8>, mut v: u64) {
    // LEB128：每字节低 7 位是数据，最高位表示后面还有
    loop {
        let byte = (v & 0x7f) as u8; // [协议] LEB128 的 7 位数据掩码
        v >>= 7; // [协议] LEB128 每字节 7 位
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80); // [协议] LEB128 续位
    }
}

struct Ser<'a> {
    out: &'a mut Vec<u8>,
}

impl Ser<'_> {
    fn str(&mut self, s: &str) {
        self.bytes(tag::STR, s.as_bytes());
    }

    fn bytes(&mut self, t: u8, b: &[u8]) {
        self.out.push(t);
        put_uint(self.out, b.len() as u64);
        self.out.extend_from_slice(b);
    }

    fn int(&mut self, v: i64) {
        if v >= 0 {
            self.out.push(tag::UINT);
            put_uint(self.out, v as u64);
        } else {
            // zigzag：-1 → 1、-2 → 3，绝对值小的负数也短
            self.out.push(tag::INT);
            put_uint(self.out, ((v << 1) ^ (v >> 63)) as u64); // [协议] zigzag 用到 64 位的符号位
        }
    }

    fn variant_map(&mut self, variant: &str) {
        self.out.push(tag::MAP);
        self.str(variant);
    }
}

impl<'a, 'b> ser::Serializer for &'a mut Ser<'b> {
    type Ok = ();
    type Error = Error;

    /// 内部二进制格式：类型据此选紧凑表示（例如附件连内容一起编码，JSON 里只有元数据）。
    fn is_human_readable(&self) -> bool {
        false
    }
    type SerializeSeq = Compound<'a, 'b>;
    type SerializeTuple = Compound<'a, 'b>;
    type SerializeTupleStruct = Compound<'a, 'b>;
    type SerializeTupleVariant = Compound<'a, 'b>;
    type SerializeMap = Compound<'a, 'b>;
    type SerializeStruct = Compound<'a, 'b>;
    type SerializeStructVariant = Compound<'a, 'b>;

    fn serialize_bool(self, v: bool) -> Result<(), Error> {
        self.out.push(if v { tag::TRUE } else { tag::FALSE });
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i16(self, v: i16) -> Result<(), Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i32(self, v: i32) -> Result<(), Error> {
        self.serialize_i64(v.into())
    }
    fn serialize_i64(self, v: i64) -> Result<(), Error> {
        self.int(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u16(self, v: u16) -> Result<(), Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u32(self, v: u32) -> Result<(), Error> {
        self.serialize_u64(v.into())
    }
    fn serialize_u64(self, v: u64) -> Result<(), Error> {
        self.out.push(tag::UINT);
        put_uint(self.out, v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), Error> {
        self.serialize_f64(v.into())
    }
    fn serialize_f64(self, v: f64) -> Result<(), Error> {
        self.out.push(tag::FLOAT);
        self.out.extend_from_slice(&v.to_le_bytes());
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), Error> {
        self.str(v.encode_utf8(&mut [0; 4])); // [协议] UTF-8 一个字符最多 4 字节
        Ok(())
    }
    fn serialize_str(self, v: &str) -> Result<(), Error> {
        self.str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), Error> {
        self.bytes(tag::BYTES, v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), Error> {
        self.out.push(tag::NULL);
        Ok(())
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), Error> {
        self.serialize_none()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<(), Error> {
        self.serialize_none()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.serialize_str(variant)
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.variant_map(variant);
        value.serialize(&mut *self)?;
        self.out.push(tag::END);
        Ok(())
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Compound<'a, 'b>, Error> {
        self.out.push(tag::SEQ);
        Ok(Compound {
            ser: self,
            variant: false,
        })
    }
    fn serialize_tuple(self, len: usize) -> Result<Compound<'a, 'b>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        len: usize,
    ) -> Result<Compound<'a, 'b>, Error> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a, 'b>, Error> {
        self.variant_map(variant);
        self.out.push(tag::SEQ);
        Ok(Compound {
            ser: self,
            variant: true,
        })
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Compound<'a, 'b>, Error> {
        self.out.push(tag::MAP);
        Ok(Compound {
            ser: self,
            variant: false,
        })
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Compound<'a, 'b>, Error> {
        self.serialize_map(Some(len))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Compound<'a, 'b>, Error> {
        self.variant_map(variant);
        self.out.push(tag::MAP);
        Ok(Compound {
            ser: self,
            variant: true,
        })
    }
}

/// map / seq 的元素写出器；`variant` 为真时外面还包着一层 `{变体名: …}`，结束时多写一个 `END`。
pub struct Compound<'a, 'b> {
    ser: &'a mut Ser<'b>,
    variant: bool,
}

impl Compound<'_, '_> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut *self.ser)
    }

    fn finish(self) -> Result<(), Error> {
        self.ser.out.push(tag::END);
        if self.variant {
            self.ser.out.push(tag::END);
        }
        Ok(())
    }
}

impl ser::SerializeSeq for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTuple for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTupleStruct for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeTupleVariant for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeMap for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.element(key)
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeStruct for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.ser.str(key);
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl ser::SerializeStructVariant for Compound<'_, '_> {
    type Ok = ();
    type Error = Error;
    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.ser.str(key);
        self.element(value)
    }
    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}
