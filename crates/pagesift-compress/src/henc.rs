//! Huffman 字面量编码：限长码、权重表（直接 4 位或 FSE 压缩）、1 流/4 流。
use crate::enc::BitWriter;
use crate::fse;
use crate::huff;

/// 限长（≤ max_len）Huffman 码长；频率超限时反复折半重建。
fn lengths(freq: &[u32; 256], max_len: u8) -> [u8; 256] {
    let mut f = *freq;
    loop {
        let mut leaves: Vec<(u32, usize)> =
            (0..256).filter(|&s| f[s] > 0).map(|s| (f[s], s)).collect();
        leaves.sort();
        let n = leaves.len();
        // 双队列法：节点 0..n 是叶子，n.. 是内部节点
        let mut weight: Vec<u64> = leaves.iter().map(|l| l.0 as u64).collect();
        let mut parent = vec![0usize; 2 * n];
        let (mut li, mut ii) = (0usize, n);
        for _ in 0..n - 1 {
            let mut pick = |weight: &Vec<u64>| -> usize {
                let leaf_ok = li < n;
                let int_ok = ii < weight.len();
                if leaf_ok && (!int_ok || weight[li] <= weight[ii]) {
                    li += 1;
                    li - 1
                } else {
                    ii += 1;
                    ii - 1
                }
            };
            let a = pick(&weight);
            let b = pick(&weight);
            let id = weight.len();
            weight.push(weight[a] + weight[b]);
            parent[a] = id;
            parent[b] = id;
        }
        let root = weight.len() - 1;
        let mut depth = vec![0u8; weight.len()];
        for i in (0..root).rev() {
            depth[i] = depth[parent[i]] + 1;
        }
        if depth[..n].iter().all(|&d| d <= max_len) {
            let mut out = [0u8; 256];
            for (k, l) in leaves.iter().enumerate() {
                out[l.1] = depth[k];
            }
            return out;
        }
        for x in f.iter_mut().filter(|x| **x > 0) {
            *x = x.div_ceil(2);
        }
    }
}

/// 把权重表写成 FSE 压缩形式；失败返回 None。
fn fse_weights(w: &[u8]) -> Option<Vec<u8>> {
    const AL: u32 = 6;
    let n = w.len();
    if n < 3 {
        return None;
    }
    let last = *w.iter().max()? as usize;
    let mut cnt = vec![0u32; last + 1];
    for &x in w {
        cnt[x as usize] += 1;
    }
    let mut probs = vec![0i32; last + 1];
    let mut sum = 0i32;
    for (i, &c) in cnt.iter().enumerate() {
        if c > 0 {
            probs[i] = (((c as u64 * 64 + n as u64 / 2) / n as u64) as i32).max(1);
            sum += probs[i];
        }
    }
    let big = (0..=last).max_by_key(|&i| probs[i])?;
    probs[big] += 64 - sum;
    if probs[big] < 1 {
        return None;
    }
    // 分布表头
    let mut h = BitWriter::new();
    h.put(AL - 5, 4);
    let (mut remaining, mut thr, mut nbits) = (65i32, 64i32, AL + 1);
    let mut s = 0;
    while remaining > 1 && s <= last {
        let x = probs[s] + 1;
        let max = 2 * thr - 1 - remaining;
        if x < max {
            h.put(x as u32, nbits - 1);
        } else if x >= thr {
            h.put((x + max) as u32, nbits);
        } else {
            h.put(x as u32, nbits);
        }
        remaining -= probs[s];
        if probs[s] == 0 {
            let mut z = 1;
            while s + z <= last && probs[s + z] == 0 {
                z += 1;
            }
            let mut r = z - 1;
            while r >= 3 {
                h.put(3, 2);
                r -= 3;
            }
            h.put(r as u32, 2);
            s += z;
        } else {
            s += 1;
        }
        while remaining < thr {
            nbits -= 1;
            thr >>= 1;
        }
    }
    if remaining != 1 {
        return None;
    }
    let head = h.flush_bytes();
    let dist: Vec<i16> = probs.iter().map(|&p| p as i16).collect();
    let t = fse::build(&dist, AL).ok()?;
    // 两条交错状态链:偶数下标一条,奇数下标一条
    let mut cells = vec![0usize; n];
    let mut sw = BitWriter::new();
    for k in (0..n).rev() {
        let sym = w[k] as usize;
        let next = if k + 2 < n { Some(cells[k + 2]) } else { None };
        let mut found = None;
        for (i, c) in t.cells.iter().enumerate() {
            if c.sym as usize != sym {
                continue;
            }
            match next {
                None => {
                    found = Some((i, 0, 0));
                    break;
                }
                Some(x) if x >= c.base as usize && x < c.base as usize + (1usize << c.nb) => {
                    found = Some((i, c.nb as u32, (x - c.base as usize) as u32));
                    break;
                }
                _ => {}
            }
        }
        let (i, nb, v) = found?;
        cells[k] = i;
        sw.put(v, nb);
    }
    sw.put(cells[1] as u32, AL);
    sw.put(cells[0] as u32, AL);
    let body = sw.finish();
    let mut out = head;
    out.extend_from_slice(&body);
    Some(out)
}

/// 生成 Huffman 压缩的字面量段（含段头）；不划算或做不到时返回 None。
pub(crate) fn encode_literals(lits: &[u8]) -> Option<Vec<u8>> {
    let n = lits.len();
    if n < 32 {
        return None;
    }
    let mut freq = [0u32; 256];
    for &b in lits {
        freq[b as usize] += 1;
    }
    let len = lengths(&freq, 11);
    let maxlen = *len.iter().max()?;
    if maxlen == 0 {
        return None;
    }
    let lastsym = (0..256).rev().find(|&s| len[s] > 0)?;
    let weight: Vec<u8> = (0..=lastsym)
        .map(|s| if len[s] > 0 { maxlen + 1 - len[s] } else { 0 })
        .collect();
    // 树描述:去掉最后一个符号的权重(隐含)
    let shown = &weight[..lastsym];
    let direct = (shown.len() <= 128).then(|| {
        let mut d = vec![127 + shown.len() as u8];
        for c in shown.chunks(2) {
            d.push(c[0] << 4 | c.get(1).copied().unwrap_or(0));
        }
        d
    });
    let fsed = fse_weights(shown).filter(|f| f.len() < 128).map(|f| {
        let mut d = vec![f.len() as u8];
        d.extend_from_slice(&f);
        d
    });
    let desc = match (direct, fsed) {
        (Some(a), Some(b)) => {
            if b.len() < a.len() {
                b
            } else {
                a
            }
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => return None,
    };
    // 自检:解码器读回的表必须与预期一致
    let (t, used) = huff::read_tree(&desc).ok()?;
    if used != desc.len() || t.max_bits != maxlen as u32 {
        return None;
    }
    // 规范码:与解码表一致的分配
    let mut code = [0u32; 256];
    let mut pos = 0u32;
    for w in 1..=maxlen {
        for s in 0..=lastsym {
            if weight[s] == w {
                let nb = maxlen + 1 - w;
                code[s] = pos >> (maxlen - nb);
                pos += 1 << (w - 1);
            }
        }
    }
    let enc = |part: &[u8]| -> Vec<u8> {
        let mut w = BitWriter::new();
        for &b in part.iter().rev() {
            w.put(code[b as usize], len[b as usize] as u32);
        }
        w.finish()
    };
    let four = n >= 256;
    let mut streams = Vec::new();
    if four {
        let seg = n.div_ceil(4);
        let parts: Vec<Vec<u8>> = lits.chunks(seg).map(enc).collect();
        if parts.len() != 4 || parts[..3].iter().any(|p| p.len() > 65535) {
            return None;
        }
        for p in &parts[..3] {
            streams.extend_from_slice(&(p.len() as u16).to_le_bytes());
        }
        for p in &parts {
            streams.extend_from_slice(p);
        }
    } else {
        streams = enc(lits);
    }
    let comp = desc.len() + streams.len();
    let (sf, bits, hl) = if !four {
        (0u32, 10, 3)
    } else if n < 1024 && comp < 1024 {
        (1, 10, 3)
    } else if n < 16384 && comp < 16384 {
        (2, 14, 4)
    } else {
        (3, 18, 5)
    };
    if comp >= (1 << bits) || n >= (1 << bits) || comp + hl >= n {
        return None;
    }
    let hv = 2u64 | (sf as u64) << 2 | (n as u64) << 4 | (comp as u64) << (4 + bits);
    let mut out = hv.to_le_bytes()[..hl].to_vec();
    out.extend_from_slice(&desc);
    out.extend_from_slice(&streams);
    Some(out)
}
