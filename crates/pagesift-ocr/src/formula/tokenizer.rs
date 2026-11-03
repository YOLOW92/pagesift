//! 分词器的解码方向（id → 文本），按 tokenizer.json：模型是 BPE，解码器是 ByteLevel
//! （GPT-2 的字节到可见字符映射）。语义同 Hugging Face tokenizers 的
//! `decode(ids, skip_special_tokens=True)`：特殊 token 跳过，各 token 的字符逐个映射回字节、
//! 拼起来按 UTF-8 解码（非法序列换成 U+FFFD）；某个 token 里有映射表外的字符时，
//! 这个 token 按它自身的 UTF-8 字节原样输出。归一化器（NFKC）和预分词只用于编码，解码不涉及。

use std::path::Path;

/// 解码用的分词表。
pub struct Tokenizer {
    /// 下标是 id：这个 token 解码出的字节；特殊 token 和词表里没有的 id 为 `None`（跳过）。
    bytes: Vec<Option<Vec<u8>>>,
}

impl Tokenizer {
    /// 读模型目录下的 tokenizer.json。
    pub fn load(dir: &Path) -> Result<Tokenizer, String> {
        Tokenizer::from_json(&crate::read_json(dir, "tokenizer.json")?)
    }

    pub fn from_json(j: &serde_json::Value) -> Result<Tokenizer, String> {
        let decoder = j["decoder"]["type"].as_str().unwrap_or("");
        if decoder != "ByteLevel" {
            return Err(format!(
                "unsupported decoder {decoder:?}, only ByteLevel is supported"
            ));
        }
        let model = j["model"]["type"].as_str().unwrap_or("");
        if model != "BPE" {
            return Err(format!(
                "unsupported tokenizer model {model:?}, only BPE is supported"
            ));
        }
        let vocab = j["model"]["vocab"]
            .as_object()
            .ok_or("tokenizer.json has no model.vocab")?;
        let table = byte_table();
        let mut bytes: Vec<Option<Vec<u8>>> = Vec::new();
        let mut put = |id: usize, v: Option<Vec<u8>>| {
            if bytes.len() <= id {
                bytes.resize(id + 1, None);
            }
            bytes[id] = v;
        };
        for (tok, id) in vocab {
            let id = id.as_u64().ok_or("a vocabulary id is not an integer")? as usize;
            put(id, Some(token_bytes(tok, &table)));
        }
        // 追加的 token 覆盖词表里的同 id 项；特殊的解码时跳过
        for a in j["added_tokens"].as_array().into_iter().flatten() {
            let id = a["id"]
                .as_u64()
                .ok_or("an added_tokens id is not an integer")? as usize;
            let content = a["content"]
                .as_str()
                .ok_or("an added_tokens entry has no content")?;
            let special = a["special"].as_bool().unwrap_or(false);
            put(id, (!special).then(|| token_bytes(content, &table)));
        }
        Ok(Tokenizer { bytes })
    }

    /// 词表大小（最大 id + 1）。
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// id 序列 → 文本，跳过特殊 token 和未知 id。
    pub fn decode(&self, ids: &[u32]) -> String {
        let mut out = Vec::new();
        for &id in ids {
            if let Some(Some(b)) = self.bytes.get(id as usize) {
                out.extend_from_slice(b);
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// 一个 token 的字节：每个字符按映射表还原成一个字节；有表外字符时取它自身的 UTF-8 字节。
fn token_bytes(tok: &str, table: &[Option<u8>]) -> Vec<u8> {
    tok.chars()
        .map(|c| table.get(c as usize).copied().flatten())
        .collect::<Option<Vec<u8>>>()
        .unwrap_or_else(|| tok.as_bytes().to_vec())
}

/// GPT-2 `bytes_to_unicode` 的逆表，下标是字符的码位：
/// 可见字节 `!`–`~`、`¡`–`¬`、`®`–`ÿ` 映射到同码位的字符，其余 68 个字节按从小到大依次映射到
/// U+0100 起的字符。
fn byte_table() -> Vec<Option<u8>> {
    // [协议] GPT-2 encoder.py 的 bytes_to_unicode：三段可见字节区间与表外字节的起始码位 256
    let visible = |b: u8| (33..=126).contains(&b) || (161..=172).contains(&b) || b >= 174;
    // [协议] 同上：表外字节从 U+0100 起
    const SHIFT: usize = 256;
    let mut table = vec![None; SHIFT * 2];
    let mut n = 0;
    for b in 0..=u8::MAX {
        if visible(b) {
            table[usize::from(b)] = Some(b);
        } else {
            table[SHIFT + n] = Some(b);
            n += 1;
        }
    }
    table.truncate(SHIFT + n);
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 小分词表：条目取自 PP-FormulaNet_plus-L 的 tokenizer.json（id 是原文件里的）。
    fn mini() -> Tokenizer {
        let j = serde_json::json!({
            "decoder": {"type": "ByteLevel", "add_prefix_space": true},
            "model": {"type": "BPE", "vocab": {
                "<s>": 0, "<pad>": 1, "</s>": 2, "(": 30, "d": 90, "\\": 82,
                "ĉ": 220, "Ċ": 221, "ĠĠ": 304, "ĠâĪĴ": 612, "âĢĵ": 853, "âĪĴ": 1059,
                "ĠÎ¼": 1173, "ä": 183, "¸": 139, "Ń": 278, "ä¸": 19598, "Ġown": 4158
            }},
            "added_tokens": [
                {"id": 0, "content": "<s>", "special": true},
                {"id": 1, "content": "<pad>", "special": true},
                {"id": 2, "content": "</s>", "special": true}
            ]
        });
        Tokenizer::from_json(&j).unwrap()
    }

    #[test]
    fn decodes_byte_level_tokens() {
        let t = mini();
        assert_eq!(t.decode(&[612]), " −");
        assert_eq!(t.decode(&[853, 1059]), "–−");
        assert_eq!(t.decode(&[1173]), " μ");
        assert_eq!(t.decode(&[304, 220, 221]), "  \t\n");
        assert_eq!(t.decode(&[82, 90, 30, 4158]), "\\d( own");
        // 一个汉字的三个字节分在三个 token 里，拼起来才是完整的 UTF-8
        assert_eq!(t.decode(&[183, 139, 278]), "中");
        // 只有前两个字节：非法序列换成 U+FFFD
        assert_eq!(t.decode(&[19598]), "\u{fffd}");
    }

    #[test]
    fn skips_special_and_unknown() {
        let t = mini();
        assert_eq!(t.decode(&[2, 0, 90, 1, 2, 99_999]), "d");
    }

    #[test]
    fn byte_table_is_a_bijection() {
        let t = byte_table();
        let mut seen = [false; 256];
        for b in t.iter().flatten() {
            assert!(!seen[usize::from(*b)]);
            seen[usize::from(*b)] = true;
        }
        assert!(seen.iter().all(|&s| s));
        // 空格 → Ġ（U+0120），换行 → Ċ（U+010A）
        assert_eq!(t['Ġ' as usize], Some(b' '));
        assert_eq!(t['Ċ' as usize], Some(b'\n'));
    }

    /// 真实的 tokenizer.json（模型目录在本机缓存里才跑）：所有条目都能解码，抽查几条。
    #[test]
    fn real_tokenizer_if_present() {
        let Some(dir) = crate::models::cache_root()
            .map(|r| r.join("PP-FormulaNet_plus-L_safetensors"))
            .filter(|d| d.join("tokenizer.json").exists())
        else {
            return;
        };
        let t = Tokenizer::load(&dir).unwrap();
        assert_eq!(t.len(), 50_000);
        assert_eq!(t.decode(&[612, 1059, 1173, 4158]), " −− μ own");
        assert_eq!(t.decode(&[183, 139, 278]), "中");
        assert_eq!(t.decode(&[0, 1, 2, 3, 22]), "");
    }
}
