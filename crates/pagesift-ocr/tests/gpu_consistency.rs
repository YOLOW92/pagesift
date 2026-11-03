//! GPU 和 CPU 逐层比较（设计 G3 的验证项，`scripts/ci.sh` 检测到显卡时跑）。
//!
//! 每层喂相同的输入（CPU 上一层的输出），最大绝对误差不超过 fp32 累加误差界 `n·ε·Σ|a||b|`：`n` 是这层
//! 每个输出元素实际累加的项数，`Σ|a||b|` 由同一层在 |a|、|b| 上重算得到；`ε = 2⁻²³` 是两边各
//! `n·u·Σ|a||b|`（`u = 2⁻²⁴`）相减后的上界。不是累加的层各用自己的界，写在对应的方法里。
//!
//! 没有显卡或没有权重时打印原因后跳过；有卡却打不开要报错。

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;

use pagesift_gpu::{Ctx, DevTensor, Owner, Worker};
use pagesift_infer::exec::{Cpu, Exec};
use pagesift_infer::{Activation, Conv2d, ConvTranspose2d, LayerNorm, Linear, Tensor, ops};
use pagesift_ocr::Image;
use pagesift_ocr::det::Det;
use pagesift_ocr::rec::Rec;

const EPS: f64 = f32::EPSILON as f64;
/// 次正规数附近 GPU 可能冲零：每项累加多留一个最小正规数。
const FLOOR: f64 = f32::MIN_POSITIVE as f64;

/// 一层的比较结果。
struct Layer {
    name: String,
    n: usize,
    err: f64,
    /// 误差 / 界，超过 1 就是失败。
    ratio: f64,
}

/// 在 CPU 上算，同时把同样的输入交给 GPU 算同一层，比较结果。
struct Check<'c, 'g> {
    ctx: &'c Ctx<'g>,
    owner: Owner,
    layers: RefCell<Vec<Layer>>,
}

fn abs(t: &Tensor) -> Tensor {
    Tensor::new(t.shape.clone(), t.data.iter().map(|v| v.abs()).collect())
}

fn abs_vec(v: &Option<Vec<f32>>) -> Option<Vec<f32>> {
    v.as_ref().map(|b| b.iter().map(|x| x.abs()).collect())
}

impl<'c, 'g> Check<'c, 'g> {
    fn exec(&self) -> pagesift_gpu::BatchExec<'c, 'g> {
        self.ctx.exec(1, self.owner.clone())
    }

    /// 比较 `gpu` 和 `cpu`，逐元素的界由 `bound(i)` 给出。
    fn record(&self, name: &str, n: usize, gpu: &[f32], cpu: &[f32], bound: impl Fn(usize) -> f64) {
        assert_eq!(gpu.len(), cpu.len(), "{name}：长度不同");
        let (mut err, mut ratio) = (0f64, 0f64);
        for (i, (&g, &c)) in gpu.iter().zip(cpu).enumerate() {
            assert_eq!(g.is_nan(), c.is_nan(), "{name}[{i}]：NaN 不一致");
            let d = (g as f64 - c as f64).abs();
            if g == c || d == 0.0 {
                continue;
            }
            err = err.max(d);
            ratio = ratio.max(d / bound(i));
        }
        self.layers.borrow_mut().push(Layer {
            name: name.to_owned(),
            n,
            err,
            ratio,
        });
    }

    /// 单输入的层：`f` 在 GPU 上算，结果下载回来。
    fn gpu1(
        &self,
        x: &Tensor,
        f: impl FnOnce(&pagesift_gpu::BatchExec<'c, 'g>, &DevTensor<'c, 'g>) -> DevTensor<'c, 'g>,
    ) -> Vec<f32> {
        let e = self.exec();
        let dx = e.upload(x);
        let y = f(&e, &dx);
        e.download(&y).expect("GPU 算这一层失败")
    }

    /// 没有舍入的层（只搬数据、比大小）：必须逐位相同。
    fn exact(&self, name: &str, gpu: &[f32], cpu: &[f32]) {
        self.record(name, 1, gpu, cpu, |_| FLOOR);
    }
}

impl Exec for Check<'_, '_> {
    type T = Tensor;

    fn shape(&self, x: &Tensor) -> Vec<usize> {
        x.shape.clone()
    }

    fn conv(&self, l: &Conv2d, x: &Tensor) -> Tensor {
        let cpu = l.forward(x);
        let g = self.gpu1(x, |e, d| e.conv(l, d));
        // 每个输出元素累加 cg·kh·kw 项，有偏置再加 1 项
        let (cg, kh, kw) = (l.weight.shape[1], l.weight.shape[2], l.weight.shape[3]);
        let n = cg * kh * kw + l.bias.is_some() as usize;
        let sum = ops::conv2d(
            &abs(x),
            &abs(&l.weight),
            abs_vec(&l.bias).as_deref(),
            l.stride,
            l.pad,
            l.dilation,
            l.groups,
        );
        let name = format!("conv {:?} g{}", l.weight.shape, l.groups);
        self.record(&name, n, &g, &cpu.data, |i| {
            n as f64 * (EPS * sum.data[i] as f64 + FLOOR)
        });
        cpu
    }

    fn conv_transpose(&self, l: &ConvTranspose2d, x: &Tensor) -> Tensor {
        let cpu = l.forward(x);
        let g = self.gpu1(x, |e, d| e.conv_transpose(l, d));
        // 每个输出元素最多累加 C·kh·kw 项，有偏置再加 1 项
        let (c, kh, kw) = (l.weight.shape[0], l.weight.shape[2], l.weight.shape[3]);
        let n = c * kh * kw + l.bias.is_some() as usize;
        let sum = ops::conv_transpose2d(
            &abs(x),
            &abs(&l.weight),
            abs_vec(&l.bias).as_deref(),
            l.stride,
            l.pad,
        );
        let name = format!("conv_transpose {:?}", l.weight.shape);
        self.record(&name, n, &g, &cpu.data, |i| {
            n as f64 * (EPS * sum.data[i] as f64 + FLOOR)
        });
        cpu
    }

    fn act(&self, a: Activation, x: &mut Tensor) {
        let input = x.clone();
        a.apply_all(x);
        let e = self.exec();
        let mut d = e.upload(&input);
        e.act(a, &mut d);
        let g = e.download(&d).expect("GPU 算激活失败");
        let name = format!("{a:?}");
        match a {
            Activation::Identity | Activation::Relu => self.exact(&name, &g, &x.data),
            // 两边同一个公式：只差一两步舍入
            Activation::HardSigmoid => self.record(&name, 1, &g, &x.data, |i| {
                4.0 * EPS * (x.data[i].abs() as f64 + 1.0) + FLOOR
            }),
            // [推导] exp 两边各约 2 ulp，加上一次除法和一次乘法：相对 8ε
            Activation::Silu | Activation::Sigmoid => self.record(&name, 1, &g, &x.data, |i| {
                8.0 * EPS * x.data[i].abs() as f64 + FLOOR
            }),
            // [推导] 两边 erf 的写法不同（CPU 用 erfc 免相消），都有 A&S 7.1.26 的 1.5e-7 绝对误差：
            // |x|·(2·1.5e-7/2 + 舍入)
            Activation::Gelu => self.record(&name, 1, &g, &x.data, |i| {
                input.data[i].abs() as f64 * (1.5e-7 + 8.0 * EPS) + FLOOR
            }),
        }
    }

    fn add(&self, a: &Tensor, b: &Tensor) -> Tensor {
        let cpu = ops::add(a, b);
        let e = self.exec();
        let (da, db) = (e.upload(a), e.upload(b));
        let g = e.download(&e.add(&da, &db)).expect("GPU 算相加失败");
        // 同样的一次 IEEE 加法，应当逐位相同
        self.exact("add", &g, &cpu.data);
        cpu
    }

    fn add_assign(&self, a: &mut Tensor, b: &Tensor) {
        let e = self.exec();
        let (mut da, db) = (e.upload(a), e.upload(b));
        e.add_assign(&mut da, &db);
        let g = e.download(&da).expect("GPU 算相加失败");
        Cpu.add_assign(a, b);
        self.exact("add_assign", &g, &a.data);
    }

    fn pad_right_bottom(&self, x: &Tensor, right: usize, bottom: usize) -> Tensor {
        let cpu = ops::pad_right_bottom(x, right, bottom);
        let g = self.gpu1(x, |e, d| e.pad_right_bottom(d, right, bottom));
        self.exact("pad_right_bottom", &g, &cpu.data);
        cpu
    }

    fn max_pool2d(
        &self,
        x: &Tensor,
        k: (usize, usize),
        stride: (usize, usize),
        ceil_mode: bool,
    ) -> Tensor {
        let cpu = ops::max_pool2d(x, k, stride, ceil_mode);
        let g = self.gpu1(x, |e, d| e.max_pool2d(d, k, stride, ceil_mode));
        self.exact("max_pool2d", &g, &cpu.data);
        cpu
    }

    fn avg_pool2d(&self, x: &Tensor, k: (usize, usize)) -> Tensor {
        let cpu = ops::avg_pool2d(x, k);
        let g = self.gpu1(x, |e, d| e.avg_pool2d(d, k));
        let n = k.0 * k.1;
        let sum = ops::avg_pool2d(&abs(x), k);
        self.record("avg_pool2d", n, &g, &cpu.data, |i| {
            n as f64 * (EPS * sum.data[i] as f64 + FLOOR)
        });
        cpu
    }

    fn global_avg(&self, x: &Tensor) -> Tensor {
        let cpu = Cpu.global_avg(x);
        let g = self.gpu1(x, |e, d| e.global_avg(d));
        let (_, h, w) = x.chw();
        let n = h * w;
        let sum = ops::global_avg_pool(&abs(x));
        self.record("global_avg", n, &g, &cpu.data, |i| {
            n as f64 * (EPS * sum[i] as f64 + FLOOR)
        });
        cpu
    }

    fn scale_channels(&self, x: &mut Tensor, s: &Tensor) {
        let e = self.exec();
        let (mut dx, ds) = (e.upload(x), e.upload(s));
        e.scale_channels(&mut dx, &ds);
        let g = e.download(&dx).expect("GPU 算逐通道乘失败");
        Cpu.scale_channels(x, s);
        self.exact("scale_channels", &g, &x.data);
    }

    fn se_gate(&self, s: &mut Tensor) {
        let e = self.exec();
        let mut d = e.upload(s);
        e.se_gate(&mut d);
        let g = e.download(&d).expect("GPU 算 SE 门失败");
        Cpu.se_gate(s);
        // 一次乘、两次加：每步相对 ε/2
        self.record("se_gate", 3, &g, &s.data, |_| 4.0 * EPS);
    }

    fn concat(&self, xs: &[&Tensor]) -> Tensor {
        let cpu = ops::concat_channels(xs);
        let e = self.exec();
        let ds: Vec<_> = xs.iter().map(|x| e.upload(x)).collect();
        let refs: Vec<_> = ds.iter().collect();
        let g = e.download(&e.concat(&refs)).expect("GPU 算拼接失败");
        self.exact("concat", &g, &cpu.data);
        cpu
    }

    fn upsample(&self, x: &Tensor, oh: usize, ow: usize) -> Tensor {
        let cpu = ops::interpolate_nearest(x, oh, ow);
        let g = self.gpu1(x, |e, d| e.upsample(d, oh, ow));
        self.exact("upsample", &g, &cpu.data);
        cpu
    }

    fn chw_to_seq(&self, x: &Tensor) -> Tensor {
        let cpu = ops::chw_to_seq(x);
        let g = self.gpu1(x, |e, d| e.chw_to_seq(d));
        self.exact("chw_to_seq", &g, &cpu.data);
        cpu
    }

    fn linear(&self, l: &Linear, x: &Tensor) -> Tensor {
        let cpu = l.forward(x);
        let g = self.gpu1(x, |e, d| e.linear(l, d));
        // 每个输出元素累加 K 项，有偏置再加 1 项
        let (_, k) = x.tc();
        let n = k + l.bias.is_some() as usize;
        let mut sum = ops::matmul(&abs(x), &abs(&l.weight_t));
        if let Some(b) = &l.bias {
            for row in sum.data.chunks_mut(b.len()) {
                for (v, b) in row.iter_mut().zip(b) {
                    *v += b.abs();
                }
            }
        }
        let name = format!("linear {:?}", l.weight_t.shape);
        self.record(&name, n, &g, &cpu.data, |i| {
            n as f64 * (EPS * sum.data[i] as f64 + FLOOR)
        });
        cpu
    }

    fn layer_norm(&self, l: &LayerNorm, x: &Tensor) -> Tensor {
        let cpu = l.forward(x);
        let g = self.gpu1(x, |e, d| e.layer_norm(l, d));
        let (_, c) = x.tc();
        // [推导] 均值累加 c 项、方差再累加 c 项，相对误差约 4c·ε，经开方和除法传到每个输出；
        // 输出 = (x − μ)·inv·w + b，所以界是 4c·ε·(|输出 − b| + |w|·inv·max|x|) + ε·|b|
        let bound: Vec<f64> = x
            .data
            .chunks(c)
            .flat_map(|row| {
                let mean = row.iter().sum::<f32>() / c as f32;
                let var = row.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / c as f32;
                let inv = 1.0 / ((var + l.eps) as f64).sqrt();
                let m = row.iter().fold(0f64, |m, v| m.max(v.abs() as f64));
                let k = 4.0 * c as f64 * EPS;
                row.iter()
                    .enumerate()
                    .map(move |(i, v)| {
                        let w = l.weight[i].abs() as f64;
                        k * w * inv * ((*v - mean).abs() as f64 + m)
                            + EPS * l.bias[i].abs() as f64
                            + FLOOR
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        self.record("layer_norm", 2 * c, &g, &cpu.data, |i| bound[i]);
        cpu
    }

    fn attention(&self, qkv: &Tensor, heads: usize) -> Tensor {
        let cpu = Cpu.attention(qkv, heads);
        let g = self.gpu1(qkv, |e, d| e.attention(d, heads));
        let (t, c3) = qkv.tc();
        let c = c3 / 3;
        let hd = c / heads;
        // [推导] 分数 q·kᵀ 累加 hd 项，绝对误差 e = hd·ε·scale·Σ|q||k|；softmax 把它放大成相对误差
        // 2·max e（分子分母各一个）再加 T 项累加和两边 exp 的舍入；a·v 再累加 T 项。所以
        // |Δo| ≤ (2·max e + (T + 8)·ε)·Σ a|v| 外加 a·v 自身的 T·ε·Σ a|v|
        let scale = pagesift_infer::exec::attention_scale(hd) as f64;
        let mut bound = vec![0f64; t * c];
        for h in 0..heads {
            let take = |s: usize| -> Vec<&[f32]> {
                qkv.data
                    .chunks(c3)
                    .map(|row| &row[s * c + h * hd..s * c + (h + 1) * hd])
                    .collect()
            };
            let (q, k, v) = (take(0), take(1), take(2));
            let mut e_max = 0f64;
            let mut a = vec![0f64; t * t];
            for i in 0..t {
                let (mut row_max, mut z) = (f64::NEG_INFINITY, 0f64);
                let raw: Vec<f64> = (0..t)
                    .map(|j| {
                        let (mut dot, mut sum) = (0f64, 0f64);
                        for d in 0..hd {
                            let (x, y) = (q[i][d] as f64, k[j][d] as f64);
                            dot += x * y;
                            sum += (x * y).abs();
                        }
                        e_max = e_max.max(hd as f64 * EPS * scale * sum);
                        dot * scale
                    })
                    .collect();
                for &r in &raw {
                    row_max = row_max.max(r);
                }
                for (j, &r) in raw.iter().enumerate() {
                    a[i * t + j] = (r - row_max).exp();
                    z += a[i * t + j];
                }
                for j in 0..t {
                    a[i * t + j] /= z;
                }
            }
            let rel = 2.0 * e_max + (2.0 * t as f64 + 8.0) * EPS;
            for i in 0..t {
                for d in 0..hd {
                    let s: f64 = (0..t).map(|j| a[i * t + j] * (v[j][d].abs() as f64)).sum();
                    bound[i * c + h * hd + d] = rel * s + t as f64 * FLOOR;
                }
            }
        }
        self.record(&format!("attention t{t} c{c}"), 2 * t, &g, &cpu.data, |i| {
            bound[i]
        });
        cpu
    }

    fn ctc_digest(&self, logits: &Tensor) -> Vec<Vec<(u32, f32)>> {
        let cpu = Cpu.ctc_digest(logits);
        let e = self.exec();
        let d = e.upload(logits);
        let g = e.ctc_digest(&d);
        e.finish().expect("GPU 算 CTC 摘要失败");
        assert_eq!(g[0].len(), cpu[0].len(), "CTC 摘要的步数不同");
        let args_same = g[0].iter().zip(&cpu[0]).all(|(a, b)| a.0 == b.0);
        assert!(args_same, "CTC 摘要的最大类不同");
        let (gp, cp): (Vec<f32>, Vec<f32>) =
            g[0].iter().zip(&cpu[0]).map(|(a, b)| (a.1, b.1)).unzip();
        // [推导] z 累加 n 项，每项 exp 差约 4 ulp：相对 (n + 8)·ε
        let n = logits.tc().1;
        self.record("ctc_digest", n, &gp, &cp, |i| {
            (n as f64 + 8.0) * EPS * cp[i] as f64 + FLOOR
        });
        cpu
    }
}

fn model_dir(repo: &str) -> Option<PathBuf> {
    let d = pagesift_ocr::models::cache_root()?.join(repo);
    d.join("model.safetensors").exists().then_some(d)
}

/// 有卡才返回工人；有卡打不开要报错（不能当成“没有 GPU”跳过）。
fn worker() -> Option<Worker> {
    match Worker::spawn(0) {
        Ok(w) => Some(w),
        Err(e) if pagesift_gpu::device_count() == 0 => {
            eprintln!("跳过 GPU 一致性测试：{e}");
            None
        }
        Err(e) => panic!("有 N 卡但打开失败：{e}"),
    }
}

/// 像文档页的合成图：白底上若干行深色的不规则块（xorshift），够让每层都有真实的数值范围。
fn page(width: usize, height: usize) -> Image {
    let mut data = vec![235u8; width * height];
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for row in (40..height.saturating_sub(40)).step_by(44) {
        let mut x = 40;
        while x < width - 60 {
            let w = 6 + (next() % 16) as usize;
            let h = 12 + (next() % 14) as usize;
            let v = (next() % 90) as u8;
            for y in row..(row + h).min(height) {
                for p in &mut data[y * width + x..y * width + (x + w).min(width)] {
                    *p = v;
                }
            }
            x += w + 2 + (next() % 10) as usize;
        }
    }
    Image {
        width,
        height,
        channels: 1,
        data,
    }
}

/// 打印各类层的最差情况，断言都在界内。
fn verify(layers: &[Layer], what: &str) {
    let mut worst: std::collections::BTreeMap<&str, &Layer> = Default::default();
    for l in layers {
        let kind = l.name.split(' ').next().unwrap_or(&l.name);
        let e = worst.entry(kind).or_insert(l);
        if l.ratio > e.ratio {
            *e = l;
        }
    }
    eprintln!("{what}：共 {} 层，各类最差（误差 / 界）：", layers.len());
    for (kind, l) in &worst {
        eprintln!(
            "  {kind:<16} n={:<6} 误差 {:.3e}  误差/界 {:.4}  {}",
            l.n, l.err, l.ratio, l.name
        );
    }
    for l in layers {
        assert!(
            l.ratio <= 1.0,
            "{what} 的 {}：误差 {:.3e} 超过累加误差界（误差/界 = {:.3}）",
            l.name,
            l.err,
            l.ratio
        );
    }
}

fn check_det(worker: &Worker, repo: &str) {
    let Some(dir) = model_dir(repo) else {
        eprintln!("跳过 {repo}：缓存里没有权重");
        return;
    };
    let det = Arc::new(Det::load(&dir).unwrap());
    let x = det.preprocess(&page(640, 480)).unwrap();
    let layers = worker
        .call(|ctx: &Ctx<'_>| {
            let owner: Owner = det.clone();
            let c = Check {
                ctx,
                owner,
                layers: RefCell::default(),
            };
            det.prob_on(&c, &x).unwrap();
            c.layers.into_inner()
        })
        .unwrap();
    verify(&layers, repo);
}

fn rec_input(rec: &Rec, w: usize) -> Tensor {
    rec.preprocess(&page(w, 48)).unwrap()
}

fn check_rec(worker: &Worker, repo: &str) {
    let Some(dir) = model_dir(repo) else {
        eprintln!("跳过 {repo}：缓存里没有权重");
        return;
    };
    let rec = Arc::new(Rec::load(&dir).unwrap());
    let x = rec_input(&rec, 400);
    let layers = worker
        .call(|ctx: &Ctx<'_>| {
            let owner: Owner = rec.clone();
            let c = Check {
                ctx,
                owner,
                layers: RefCell::default(),
            };
            let logits = rec.logits_on(&c, &x);
            c.ctc_digest(&logits);
            c.layers.into_inner()
        })
        .unwrap();
    verify(&layers, repo);
}

#[test]
fn small_det_逐层一致() {
    if let Some(w) = worker() {
        check_det(&w, "PP-OCRv6_small_det_safetensors");
    }
}

#[test]
fn small_rec_逐层一致() {
    if let Some(w) = worker() {
        check_rec(&w, "PP-OCRv6_small_rec_safetensors");
    }
}

#[test]
fn medium_det_逐层一致() {
    if let Some(w) = worker() {
        check_det(&w, "PP-OCRv6_medium_det_safetensors");
    }
}

#[test]
fn medium_rec_逐层一致() {
    if let Some(w) = worker() {
        check_rec(&w, "PP-OCRv6_medium_rec_safetensors");
    }
}

/// 拼批的结果要和单张在 CPU 上算的一样：宽度不同的行拼在一起，文字逐字相同，置信度只差舍入。
#[test]
fn 拼批和单张一致() {
    let Some(w) = worker() else { return };
    let Some(dir) = model_dir("PP-OCRv6_small_rec_safetensors") else {
        eprintln!("跳过：缓存里没有识别权重");
        return;
    };
    let rec = Arc::new(Rec::load(&dir).unwrap());
    // 窄于 320 的会补到 320；其余各不相同，含奇数宽度（池化向下取整）
    let widths = [120, 320, 321, 500, 733, 1000];
    let xs: Vec<Tensor> = widths.iter().map(|&v| rec_input(&rec, v)).collect();
    let refs: Vec<&Tensor> = xs.iter().collect();
    let digests = w
        .call(|ctx: &Ctx<'_>| {
            let owner: Owner = rec.clone();
            let e = ctx.exec(refs.len(), owner);
            let x = e.upload_batch(&refs);
            let logits = rec.logits_on(&e, &x);
            let rows = e.ctc_digest(&logits);
            e.finish().unwrap();
            rows
        })
        .unwrap();
    for ((x, d), width) in xs.iter().zip(&digests).zip(widths) {
        let logits = rec.logits_on(&Cpu, x);
        let cpu = &Cpu.ctc_digest(&logits)[0];
        assert_eq!(d.len(), cpu.len(), "宽 {width}：序列长度不同");
        let (t_g, s_g) = rec.decode(d);
        let (t_c, s_c) = rec.decode(cpu);
        assert_eq!(t_g, t_c, "宽 {width}：文字不同");
        // [策略] 多层累积后的舍入差：千分之一的置信度不改变任何阈值判断
        assert!(
            (s_g - s_c).abs() < 1e-3,
            "宽 {width}：置信度 {s_g} 与 {s_c}"
        );
        let moved = d.iter().zip(cpu).filter(|(a, b)| a.0 != b.0).count();
        assert_eq!(moved, 0, "宽 {width}：{moved} 步的最大类不同");
    }
}
