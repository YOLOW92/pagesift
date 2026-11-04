//! safetensors 权重文件：8 字节小端头长度、JSON 头（各张量的 dtype、shape、data_offsets）、
//! 紧接着的原始数据（https://github.com/huggingface/safetensors 的格式说明）。

use std::collections::HashMap;

use crate::Tensor;

/// 按名字取张量。
#[derive(Debug, Default)]
pub struct Weights {
    map: HashMap<String, Tensor>,
}

impl Weights {
    pub fn from_bytes(b: &[u8]) -> Result<Weights, String> {
        // [协议] 头长度是 8 字节小端 u64
        let n =
            u64::from_le_bytes(b.get(..8).ok_or("file too short")?.try_into().unwrap()) as usize;
        // 载入时不校验哈希，大小对但内容坏的文件也会走到这里：坏头报错，不能 panic
        let start = n.checked_add(8).ok_or("header length out of bounds")?; // [协议] 头长度字段 8 字节
        let head = b.get(8..start).ok_or("header length out of bounds")?; // [协议] 头长度字段 8 字节
        let data = &b[start..];
        let json: serde_json::Value =
            serde_json::from_slice(head).map_err(|e| format!("header is not valid JSON: {e}"))?;
        let obj = json.as_object().ok_or("header is not a JSON object")?;
        let mut map = HashMap::new();
        for (name, v) in obj {
            if name == "__metadata__" {
                continue;
            }
            let dtype = v["dtype"]
                .as_str()
                .ok_or_else(|| format!("{name} has no dtype"))?;
            let shape: Vec<usize> = v["shape"]
                .as_array()
                .ok_or_else(|| format!("{name} has no shape"))?
                .iter()
                .map(|d| {
                    d.as_u64()
                        .map(|d| d as usize)
                        .ok_or("shape is not an integer")
                })
                .collect::<Result<_, _>>()?;
            let off = v["data_offsets"]
                .as_array()
                .ok_or_else(|| format!("{name} has no data_offsets"))?;
            let offset = |i: usize| {
                off.get(i)
                    .and_then(serde_json::Value::as_u64)
                    .map(|v| v as usize)
                    .ok_or_else(|| format!("{name}: data_offsets needs two integers"))
            };
            let (s, e) = (offset(0)?, offset(1)?);
            let raw = data
                .get(s..e)
                .ok_or_else(|| format!("{name} data out of bounds"))?;
            let values: Vec<f32> = match dtype {
                // [协议] F32 是 4 字节小端 IEEE 754
                "F32" => raw
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&c| f32::from_le_bytes(c))
                    .collect(),
                // [协议] F16、BF16 是 2 字节小端
                "F16" => raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| f16_to_f32(u16::from_le_bytes(c)))
                    .collect(),
                // [协议] BF16 是 f32 的高 16 位
                "BF16" => raw
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&c| f32::from_bits(u32::from(u16::from_le_bytes(c)) << 16))
                    .collect(),
                _ => return Err(format!("{name}: unsupported dtype {dtype}")),
            };
            let want = shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d));
            if want != Some(values.len()) {
                return Err(format!("{name}: data length does not match the shape"));
            }
            map.insert(name.clone(), Tensor::new(shape, values));
        }
        Ok(Weights { map })
    }

    pub fn get(&self, name: &str) -> Result<&Tensor, String> {
        self.map
            .get(name)
            .ok_or_else(|| format!("no {name} in the weights"))
    }

    pub fn opt(&self, name: &str) -> Option<&Tensor> {
        self.map.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }
}

/// IEEE 754 半精度转单精度。
fn f16_to_f32(h: u16) -> f32 {
    // [协议] 半精度：1 位符号、5 位指数（偏移 15）、10 位尾数
    let s = u32::from(h >> 15) << 31;
    let e = u32::from((h >> 10) & 0x1F);
    let m = u32::from(h & 0x3FF);
    let bits = match e {
        0 if m == 0 => s,
        0 => {
            // 非规格化数：规格化后再拼
            let mut e = 127 - 15 + 1;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            s | (e << 23) | ((m & 0x3FF) << 13)
        }
        0x1F => s | 0x7F80_0000 | (m << 13),
        _ => s | ((e + 127 - 15) << 23) | (m << 13),
    };
    f32::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 8 字节头长度加 JSON 头加数据。
    fn file(head: &str, data: &[u8]) -> Vec<u8> {
        let mut b = (head.len() as u64).to_le_bytes().to_vec();
        b.extend_from_slice(head.as_bytes());
        b.extend_from_slice(data);
        b
    }

    #[test]
    fn reads_a_good_file() {
        let b = file(
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,4]}}"#,
            &1f32.to_le_bytes(),
        );
        let w = Weights::from_bytes(&b).unwrap();
        assert_eq!(w.get("a").unwrap().data, [1.0]);
    }

    /// 坏头都是错误，不是 panic。
    #[test]
    fn bad_headers_are_errors() {
        for head in [
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[]}}"#,
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0]}}"#,
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,"4"]}}"#,
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[4,0]}}"#,
            r#"{"a":{"dtype":"F32","shape":[4294967296,4294967296],"data_offsets":[0,4]}}"#,
            r#"{"a":[]}"#,
            "[]",
        ] {
            let e = Weights::from_bytes(&file(head, &[0; 4])).unwrap_err();
            assert!(!e.is_empty(), "{head}");
        }
        // 头长度声明到 u64 的上限：加 8 不能溢出
        assert!(Weights::from_bytes(&u64::MAX.to_le_bytes()).is_err());
    }
}
