//! M1：把 tree-sitter 生成的 `parser.c` 转成 [`Language`] 表。

use crate::cdata::{Key, Src, Tok, Val};
use crate::lexconv::convert_lex;
use crate::table::{Action, Language};
use std::collections::HashMap;

fn idx_vec<T: Clone>(
    v: &Val,
    default: T,
    f: impl Fn(&Val) -> Result<T, String>,
) -> Result<Vec<T>, String> {
    let mut out: Vec<T> = Vec::new();
    for (k, x) in v.list() {
        if let Key::Idx(i) = k {
            let i = *i as usize;
            if i >= out.len() {
                out.resize(i + 1, default.clone());
            }
            out[i] = f(x)?;
        }
    }
    Ok(out)
}

fn as_u16(v: &Val) -> Result<u16, String> {
    match v {
        Val::Int(i) => Ok(*i as u16),
        Val::Call(_, a) if a.len() == 1 => as_u16(&a[0]),
        o => Err(format!("期望整数，遇到 {o:?}")),
    }
}

fn def(src: &Src, name: &str) -> u32 {
    src.consts.get(name).copied().unwrap_or(0) as u32
}

/// 转换一份 `parser.c` 的全文。`fallback_name` 在源码里没有 `.name` 时使用。
pub fn convert(text: &str, fallback_name: &str) -> Result<Language, String> {
    let mut src = Src::new(text)?;
    let mut l = Language {
        name: fallback_name.to_string(),
        ..Default::default()
    };
    // 官方 SMALL_STATE 宏依赖 LARGE_STATE_COUNT
    l.abi_version = def(&src, "LANGUAGE_VERSION");
    l.symbol_count = def(&src, "SYMBOL_COUNT");
    l.alias_count = def(&src, "ALIAS_COUNT");
    l.token_count = def(&src, "TOKEN_COUNT");
    l.external_token_count = def(&src, "EXTERNAL_TOKEN_COUNT");
    l.state_count = def(&src, "STATE_COUNT");
    l.large_state_count = def(&src, "LARGE_STATE_COUNT");
    l.production_id_count = def(&src, "PRODUCTION_ID_COUNT");
    l.field_count = def(&src, "FIELD_COUNT");
    l.max_alias_sequence_length = def(&src, "MAX_ALIAS_SEQUENCE_LENGTH");
    // 调用名到常量的转换：STATE/ACTIONS 取原值，SMALL_STATE 减去大状态数
    let lsc = l.large_state_count as i64;
    let total = (l.symbol_count + l.alias_count) as usize;

    let names = src.array("ts_symbol_names")?;
    l.symbol_names = idx_vec(&names, String::new(), |v| match v {
        Val::Str(s) => Ok(s.clone()),
        _ => Ok(String::new()),
    })?;
    l.symbol_names.resize(total, String::new());
    l.public_symbol_map = idx_vec(&src.array("ts_symbol_map")?, 0u16, as_u16)?;
    l.public_symbol_map.resize(total, 0);
    let meta = src.array("ts_symbol_metadata")?;
    l.symbol_metadata = idx_vec(&meta, 0u8, |v| {
        let flag = |n: &str| v.field(n).and_then(|x| x.int()).unwrap_or(0) != 0;
        Ok(flag("visible") as u8 | (flag("named") as u8) << 1 | (flag("supertype") as u8) << 2)
    })?;
    l.symbol_metadata.resize(total, 0);
    // 没有字段的语法不生成 ts_field_names
    if src.find_init("ts_field_names").is_some() {
        l.field_names = idx_vec(&src.array("ts_field_names")?, String::new(), |v| match v {
            Val::Str(s) => Ok(s.clone()),
            _ => Ok(String::new()),
        })?;
    }
    if let Some(p) = src.find_init("ts_field_map_slices") {
        let v = src.parse_init_at(p)?;
        l.field_map_slices = idx_vec(&v, (0u16, 0u16), |s| {
            let g = |n: &str| s.field(n).and_then(|x| x.int()).unwrap_or(0) as u16;
            Ok((g("index"), g("length")))
        })?;
    }
    l.field_map_slices
        .resize(l.production_id_count as usize, (0, 0));
    if let Some(p) = src.find_init("ts_field_map_entries") {
        let v = src.parse_init_at(p)?;
        for (_, e) in v.list() {
            let items = e.list();
            let g = |i: usize| items.get(i).and_then(|x| x.1.int()).unwrap_or(0);
            let inh = e.field("inherited").and_then(|x| x.int()).unwrap_or(0) != 0;
            l.field_map_entries.push((g(0) as u16, g(1) as u8, inh));
        }
    }
    // alias_sequences[prod][pos]
    let m = l.max_alias_sequence_length as usize;
    l.alias_sequences = vec![0; m * l.production_id_count as usize];
    if let Some(p) = src.find_init("ts_alias_sequences") {
        let v = src.parse_init_at(p)?;
        for (k, row) in v.list() {
            if let Key::Idx(r) = k {
                for (k2, x) in row.list() {
                    if let Key::Idx(c) = k2 {
                        let pos = *r as usize * m + *c as usize;
                        if pos < l.alias_sequences.len() {
                            l.alias_sequences[pos] = as_u16(x)?;
                        }
                    }
                }
            }
        }
    }
    l.non_terminal_alias_map = idx_vec(&src.array("ts_non_terminal_alias_map")?, 0u16, as_u16)?;
    l.primary_state_ids = idx_vec(&src.array("ts_primary_state_ids")?, 0u16, as_u16)?;
    let sc = l.state_count as usize;
    let lm = src.array("ts_lex_modes")?;
    l.lex_modes = idx_vec(&lm, 0u16, |v| {
        // 无词法状态的项写成位置初始化 `{(TSStateId)(-1),}`
        let x = v
            .field("lex_state")
            .or_else(|| v.list().first().map(|e| &e.1));
        Ok(x.and_then(|x| x.int()).unwrap_or(0) as u16)
    })?;
    l.lex_modes.resize(sc, 0);
    if l.external_token_count > 0 {
        l.lex_ext = idx_vec(&lm, 0u16, |v| {
            Ok(v.field("external_lex_state")
                .and_then(|x| x.int())
                .unwrap_or(0) as u16)
        })?;
        l.lex_ext.resize(sc, 0);
        let ec = l.external_token_count as usize;
        l.ext_symbol_map = idx_vec(&src.array("ts_external_scanner_symbol_map")?, 0u16, as_u16)?;
        l.ext_symbol_map.resize(ec, 0);
        let st = src.array("ts_external_scanner_states")?;
        for (k, row) in st.list() {
            if let Key::Idx(r) = k {
                let need = (*r as usize + 1) * ec;
                if l.ext_valid.len() < need {
                    l.ext_valid.resize(need, false);
                }
                for (k2, x) in row.list() {
                    if let Key::Idx(c) = k2 {
                        if (*c as usize) < ec {
                            l.ext_valid[*r as usize * ec + *c as usize] = x.int().unwrap_or(0) != 0;
                        }
                    }
                }
            }
        }
        let rows = l.lex_ext.iter().map(|x| *x as usize + 1).max().unwrap_or(1);
        if l.ext_valid.len() < rows * ec {
            l.ext_valid.resize(rows * ec, false);
        }
    }
    l.reserved_set = idx_vec(&lm, 0u8, |v| {
        Ok(v.field("reserved_word_set_id")
            .and_then(|x| x.int())
            .unwrap_or(0) as u8)
    })?;
    l.reserved_set.resize(sc, 0);
    l.max_reserved = def(&src, "MAX_RESERVED_WORD_SET_SIZE");
    if l.max_reserved > 0 {
        if let Some(p) = src.find_init("ts_reserved_words") {
            let v = src.parse_init_at(p)?;
            let m = l.max_reserved as usize;
            for (k, row) in v.list() {
                if let Key::Idx(r) = k {
                    let need = (*r as usize + 1) * m;
                    if l.reserved_words.len() < need {
                        l.reserved_words.resize(need, 0);
                    }
                    for (k2, x) in row.list() {
                        if let Key::Idx(c) = k2 {
                            l.reserved_words[*r as usize * m + *c as usize] = as_u16(x)?;
                        }
                    }
                }
            }
        }
    }

    // 解析表：大状态是稠密二维数组，小状态是分组列表
    let mut per_state: Vec<Vec<(u16, u16)>> = vec![Vec::new(); sc];
    let big = src.array("ts_parse_table")?;
    for (k, row) in big.list() {
        if let Key::Idx(s) = k {
            for (k2, x) in row.list() {
                if let Key::Idx(sym) = k2 {
                    let v = as_u16(x)?;
                    if v != 0 {
                        per_state[*s as usize].push((*sym as u16, v));
                    }
                }
            }
        }
    }
    let small = flat_ints(&src, "ts_small_parse_table")?;
    let map = flat_ints(&src, "ts_small_parse_table_map")?;
    for (i, off) in map.iter().enumerate() {
        let s = i + lsc as usize;
        if s >= sc {
            break;
        }
        let mut p = *off as usize;
        let groups = small[p];
        p += 1;
        for _ in 0..groups {
            let value = small[p] as u16;
            let n = small[p + 1] as usize;
            p += 2;
            for j in 0..n {
                per_state[s].push((small[p + j] as u16, value));
            }
            p += n;
        }
    }
    l.state_start.push(0);
    for st in &mut per_state {
        st.sort_unstable();
        st.dedup_by_key(|x| x.0);
        for (sym, v) in st.iter() {
            l.entries.push((*sym as u32) << 16 | *v as u32);
        }
        l.state_start.push(l.entries.len() as u32);
    }

    // 动作表：保持官方下标
    let acts = src.array("ts_parse_actions")?;
    let mut flat: Vec<(usize, Action)> = Vec::new();
    let mut max_idx = 0;
    for (k, v) in acts.list() {
        let Key::Idx(i) = k else { continue };
        let i = *i as usize;
        let a = match v {
            Val::List(_) => {
                let e = v.field("entry").ok_or("动作头缺少 entry")?;
                Action::Header(e.field("count").and_then(|x| x.int()).unwrap_or(0) as u16)
            }
            Val::Call(n, a) => {
                let g = |j: usize| a.get(j).and_then(|x| x.int()).unwrap_or(0);
                match n.as_str() {
                    "SHIFT" => Action::Shift {
                        state: g(0) as u16,
                        extra: false,
                        repetition: false,
                    },
                    "SHIFT_REPEAT" => Action::Shift {
                        state: g(0) as u16,
                        extra: false,
                        repetition: true,
                    },
                    "SHIFT_EXTRA" => Action::Shift {
                        state: 0,
                        extra: true,
                        repetition: false,
                    },
                    "REDUCE" => Action::Reduce {
                        symbol: g(0) as u16,
                        count: g(1) as u16,
                        dprec: g(2) as i16,
                        prod: g(3) as u16,
                    },
                    "RECOVER" => Action::Recover,
                    "ACCEPT_INPUT" => Action::Accept,
                    o => return Err(format!("未知动作宏 {o}")),
                }
            }
            o => return Err(format!("动作表里的意外项 {o:?}")),
        };
        max_idx = max_idx.max(i);
        flat.push((i, a));
    }
    l.actions = vec![Action::Header(0); max_idx + 1];
    for (i, a) in flat {
        l.actions[i] = a;
    }

    let mut sets = HashMap::new();
    l.lex = convert_lex(&src, "ts_lex", &mut sets)?;
    l.keyword_lex = convert_lex(&src, "ts_lex_keywords", &mut sets)?;

    // 语言结构体里的关键词捕获符号和名字
    for i in 0..src.toks.len().saturating_sub(3) {
        if src.toks[i] == Tok::P(".") {
            match (&src.toks[i + 1], &src.toks[i + 2], &src.toks[i + 3]) {
                (Tok::Id(f), Tok::P("="), Tok::Id(v)) if f == "keyword_capture_token" => {
                    l.keyword_capture_token = *src.consts.get(v).ok_or("关键词符号未知")? as u16;
                }
                (Tok::Id(f), Tok::P("="), Tok::Str(v)) if f == "name" => l.name = v.clone(),
                _ => {}
            }
        }
    }
    let _ = &mut src;
    Ok(l)
}

/// 平铺的整数数组（`small_parse_table` 等），元素可带 `[idx] =`。
fn flat_ints(src: &Src, name: &str) -> Result<Vec<u32>, String> {
    let v = src.array(name)?;
    let mut out = Vec::new();
    for (k, x) in v.list() {
        let Key::Idx(i) = k else { continue };
        let i = *i as usize;
        if i >= out.len() {
            out.resize(i + 1, 0);
        }
        out[i] = match x {
            Val::Int(n) => *n as u32,
            Val::Call(_, a) => a.first().and_then(|y| y.int()).unwrap_or(0) as u32,
            o => return Err(format!("{name}: 意外项 {o:?}")),
        };
    }
    Ok(out)
}
