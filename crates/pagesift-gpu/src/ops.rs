//! 与 `pagesift_infer::ops` 同语义的 GPU 算子。两层入口：
//! - `*_device`：输入输出都在显存，连续多个算子时不来回拷贝；
//! - `matmul`、`conv2d`、`activate`：收发主机内存，参数和返回布局同 CPU 版，方便直接替换与对照。
//!
//! 卷积的走法：每组一个输入通道（深度卷积）用直接内核；1×1、步长 1、不补边时输入本身就是 GEMM 的
//! B 矩阵；其余先 im2col 再批量 SGEMM（批次 = groups），最后加偏置。

use pagesift_infer::{Activation, Tensor};

use crate::Gpu;
use crate::buffer::{Args, DeviceBuffer};
use crate::driver::DevPtr;

/// 一维内核每块的线程数，与 `kernels/src/lib.rs` 的约定一致。
pub(crate) const BLOCK_1D: u32 = 256; // [策略] 256 线程/块：Pascal 每 SM 可驻留 2048 线程，8 块就能占满
/// SGEMM 每块算的 C 子块边长与线程块边长，与 `kernels/src/gemm.rs` 的 BM/BN 和 16×16 线程一致。
const GEMM_TILE: usize = 64; // [推导] kernels/src/gemm.rs：BM = BN = 64
const GEMM_SIDE: u32 = 16; // [推导] kernels/src/gemm.rs：每块 16×16 线程
/// 网格 y、z 维的上限。
const GRID_YZ_MAX: usize = 65535; // [协议] CUDA 编程指南“算力表”：gridDim.y、gridDim.z ≤ 65535

/// 一次卷积的全部尺寸，与 `pagesift_infer::ops::conv2d` 的参数一一对应：
/// 输入 `[c, h, w]`，权重 `[o, c/groups, kh, kw]`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvShape {
    pub c: usize,
    pub h: usize,
    pub w: usize,
    pub o: usize,
    pub kh: usize,
    pub kw: usize,
    pub stride: (usize, usize),
    pub pad: (usize, usize),
    pub dilation: (usize, usize),
    pub groups: usize,
}

impl ConvShape {
    /// 输出的 (OH, OW)，公式同 PyTorch。
    pub fn out_hw(&self) -> (usize, usize) {
        let oh =
            (self.h + 2 * self.pad.0 - self.dilation.0 * (self.kh - 1) - 1) / self.stride.0 + 1;
        let ow =
            (self.w + 2 * self.pad.1 - self.dilation.1 * (self.kw - 1) - 1) / self.stride.1 + 1;
        (oh, ow)
    }

    /// 要不要先 im2col：深度卷积走直接内核，1×1、步长 1、不补边时输入本身就是 GEMM 的 B。
    pub(crate) fn needs_col(&self) -> bool {
        let depthwise = self.c == self.groups;
        let direct = self.kh == 1 && self.kw == 1 && self.stride == (1, 1) && self.pad == (0, 0);
        !depthwise && !direct
    }

    /// 能不能走隐式卷积：步长 1、无空洞、不分组的普通卷积。输入补边后本身就是 GEMM 的 B，按核位置偏移指针，
    /// 拆成 kh·kw 次累加式 SGEMM。比 im2col 省下展开的显存（9×9 时差约 80 倍）和搬数据的时间。
    pub(crate) fn implicit(&self) -> bool {
        self.groups == 1
            && self.stride == (1, 1)
            && self.dilation == (1, 1)
            && self.c > 1
            && self.kh * self.kw > 1
    }

    /// 单个样本 im2col 展开后的元素数。
    pub(crate) fn col_len(&self) -> usize {
        let (oh, ow) = self.out_hw();
        self.c * self.kh * self.kw * oh * ow
    }

    /// 与 CPU 版相同的形状检查（不合法就 panic，属于调用方的编程错误）。
    fn check(&self) {
        assert!(
            self.groups > 0 && self.c.is_multiple_of(self.groups),
            "卷积输入通道 {} 不能被 groups {} 整除",
            self.c,
            self.groups
        );
        assert_eq!(
            self.o % self.groups,
            0,
            "输出通道 {} 不能被 groups {} 整除",
            self.o,
            self.groups
        );
        assert!(
            self.stride.0 > 0 && self.stride.1 > 0 && self.dilation.0 > 0 && self.dilation.1 > 0
        );
        assert!(self.kh > 0 && self.kw > 0, "卷积核为空");
        assert!(
            self.h + 2 * self.pad.0 > self.dilation.0 * (self.kh - 1)
                && self.w + 2 * self.pad.1 > self.dilation.1 * (self.kw - 1),
            "卷积核比补边后的输入还大"
        );
    }
}

/// `n` 个 f32 的字节数，用于设备地址的偏移。
fn bytes_of(n: usize) -> u64 {
    crate::pool::bytes(n)
}

pub(crate) fn blocks_1d(n: usize) -> u32 {
    u32::try_from(n.div_ceil(BLOCK_1D as usize)).expect("一维网格超出 u32")
}

impl Gpu {
    /// 批量 SGEMM：对 z ∈ 0..batch，`C_z[m×n] = A_z[m×k] · B_z[k×n]`，`A_z = a + z·sa`（单位：元素）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sgemm_raw(
        &self,
        a: DevPtr,
        b: DevPtr,
        c: DevPtr,
        (m, n, k): (usize, usize, usize),
        batch: usize,
        strides: (usize, usize, usize),
    ) -> Result<(), String> {
        self.sgemm_ext(a, b, c, (m, n, k), n, false, batch, strides)
    }

    /// 同 [`Gpu::sgemm_raw`]，B 的行距是 `ldb`，`accumulate` 时结果累加进 C。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn sgemm_ext(
        &self,
        a: DevPtr,
        b: DevPtr,
        c: DevPtr,
        (m, n, k): (usize, usize, usize),
        ldb: usize,
        accumulate: bool,
        batch: usize,
        (sa, sb, sc): (usize, usize, usize),
    ) -> Result<(), String> {
        let gy = m.div_ceil(GEMM_TILE);
        if gy > GRID_YZ_MAX || batch > GRID_YZ_MAX {
            return Err(format!("SGEMM grid too large: M = {m}, batch {batch}"));
        }
        let grid = (
            u32::try_from(n.div_ceil(GEMM_TILE)).map_err(|e| e.to_string())?,
            gy as u32,
            batch as u32,
        );
        let args = Args::default()
            .raw_ptr(a)
            .raw_ptr(b)
            .raw_ptr(c)
            .u32(m)
            .u32(n)
            .u32(k)
            .u32(ldb)
            .u32(accumulate as usize)
            .u64(sa)
            .u64(sb)
            .u64(sc);
        self.launch(self.k.sgemm, grid, (GEMM_SIDE, GEMM_SIDE, 1), args)
    }

    /// `c[m×n] = a[m×k] · b[k×n]`，三者都在显存、行优先。
    pub fn gemm_device(
        &self,
        a: &DeviceBuffer<'_>,
        b: &DeviceBuffer<'_>,
        c: &mut DeviceBuffer<'_>,
        m: usize,
        n: usize,
        k: usize,
    ) -> Result<(), String> {
        assert!(
            a.len() == m * k && b.len() == k * n && c.len() == m * n,
            "矩阵乘的显存长度与 m、n、k 不符"
        );
        self.sgemm_raw(a.ptr(), b.ptr(), c.ptr(), (m, n, k), 1, (0, 0, 0))
    }

    /// 二维卷积，输入、权重、偏置、输出都在显存。返回 `[o, OH, OW]` 的新显存。
    pub fn conv2d_device(
        &self,
        x: &DeviceBuffer<'_>,
        w: &DeviceBuffer<'_>,
        bias: Option<&DeviceBuffer<'_>>,
        s: &ConvShape,
    ) -> Result<DeviceBuffer<'_>, String> {
        let (oh, ow) = s.out_hw();
        let mut out = self.alloc(s.o * oh * ow)?;
        self.conv2d_device_into(x, w, bias, s, &mut out)?;
        Ok(out)
    }

    /// 同 [`Gpu::conv2d_device`]，结果写进调用方给的显存 `out`（长度 `o·OH·OW`），省一次分配；
    /// 连续推理时可以复用输出缓冲。im2col 的临时显存仍在每次调用里分配。
    pub fn conv2d_device_into(
        &self,
        x: &DeviceBuffer<'_>,
        w: &DeviceBuffer<'_>,
        bias: Option<&DeviceBuffer<'_>>,
        s: &ConvShape,
        out: &mut DeviceBuffer<'_>,
    ) -> Result<(), String> {
        s.check();
        let cg = s.c / s.groups;
        assert_eq!(x.len(), s.c * s.h * s.w, "输入长度与形状不符");
        assert_eq!(w.len(), s.o * cg * s.kh * s.kw, "权重长度与形状不符");
        assert!(
            bias.is_none_or(|b| b.len() == s.o),
            "偏置长度与输出通道数不符"
        );
        let (oh, ow) = s.out_hw();
        let ohw = oh * ow;
        assert_eq!(out.len(), s.o * ohw, "输出显存长度与形状不符");
        // 卷积内核用 u32 拆下标（kernels/src/conv.rs），输出与 im2col 展开的元素数都要小于 2³²
        let col_len = s.c * s.kh * s.kw * ohw;
        if out.len() > u32::MAX as usize || col_len > u32::MAX as usize {
            return Err(format!(
                "convolution too large: {} output and {col_len} unfolded elements exceed 2^32",
                out.len()
            ));
        }
        let col = if s.needs_col() {
            Some(self.alloc(col_len)?)
        } else {
            None
        };
        self.conv_raw(
            x.ptr(),
            w.ptr(),
            bias.map(|b| b.ptr()),
            s,
            1,
            out.ptr(),
            col.as_ref().map(|c| c.ptr()),
        )?;
        // col 在这里释放：cuMemFree 会等用到它的内核跑完
        drop(col);
        Ok(())
    }

    /// 一批 `n` 个样本的卷积，全部用设备地址：`x` 是 `[n, c, h, w]`，`out` 是 `[n, o, OH, OW]`，
    /// 权重和偏置各样本共用；`col` 是 im2col 的临时缓冲（`n·col_len` 个元素），不需要时为 None。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn conv_raw(
        &self,
        x: DevPtr,
        w: DevPtr,
        bias: Option<DevPtr>,
        s: &ConvShape,
        n: usize,
        out: DevPtr,
        col: Option<DevPtr>,
    ) -> Result<(), String> {
        s.check();
        let cg = s.c / s.groups;
        let og = s.o / s.groups;
        let (oh, ow) = s.out_hw();
        let ohw = oh * ow;
        let total = n * s.o * ohw;
        let geom = |a: Args| {
            a.u32(s.h)
                .u32(s.w)
                .u32(s.kh)
                .u32(s.kw)
                .u32(s.stride.0)
                .u32(s.stride.1)
                .u32(s.pad.0)
                .u32(s.pad.1)
                .u32(s.dilation.0)
                .u32(s.dilation.1)
                .u32(oh)
                .u32(ow)
        };
        if cg == 1 {
            // 深度卷积：直接内核，偏置在内核里加；权重、偏置按 oc % o 取，各样本共用
            let args = Args::default()
                .raw_ptr(x)
                .raw_ptr(w)
                .raw_ptr(bias.unwrap_or(0))
                .raw_ptr(out)
                .u32(n * s.o)
                .u32(og)
                .u32(s.o)
                .u32(bias.is_some() as usize);
            return self.launch(
                self.k.depthwise,
                (blocks_1d(total), 1, 1),
                (BLOCK_1D, 1, 1),
                geom(args),
            );
        }
        let kk = cg * s.kh * s.kw; // GEMM 的 K：每组一个输出元素用到的输入个数
        let b = match col {
            None => x, // 1×1：x 的 [c, h·w] 本身就是 B
            Some(col) => {
                let len = n * s.col_len();
                let args = geom(Args::default().raw_ptr(x).raw_ptr(col).u32(n * s.c));
                self.launch(
                    self.k.im2col,
                    (blocks_1d(len), 1, 1),
                    (BLOCK_1D, 1, 1),
                    args,
                )?;
                col
            }
        };
        // 组 g：A = 权重第 g·og 行起的 [og, kk]；B = 第 g·kk 行起的 [kk, ohw]；C = 输出第 g·og 个通道起
        let (sb, sc) = (kk * ohw, og * ohw);
        if s.groups == 1 {
            // 批次沿 z 维：权重不动（sa = 0），B、C 按样本走
            self.sgemm_raw(w, b, out, (og, ohw, kk), n, (0, sb, sc))?;
        } else {
            // 分组又有批次：每个样本一次，A 的步长已被组占用
            for z in 0..n {
                let off = |per: usize| bytes_of(z * s.groups * per);
                self.sgemm_raw(
                    w,
                    b + off(sb),
                    out + off(sc),
                    (og, ohw, kk),
                    s.groups,
                    (og * kk, sb, sc),
                )?;
            }
        }
        if let Some(bias) = bias {
            let args = Args::default()
                .raw_ptr(out)
                .raw_ptr(bias)
                .u32(ohw)
                .u32(s.o)
                .u32(total);
            self.launch(
                self.k.add_bias,
                (blocks_1d(total), 1, 1),
                (BLOCK_1D, 1, 1),
                args,
            )?;
        }
        Ok(())
    }

    /// 原地激活，语义同 `Activation::apply`。
    pub fn activate_device(&self, x: &mut DeviceBuffer<'_>, act: Activation) -> Result<(), String> {
        let f = match act {
            Activation::Identity => return Ok(()),
            Activation::Relu => self.k.relu,
            Activation::Silu => self.k.silu,
            Activation::Gelu => self.k.gelu,
            Activation::HardSigmoid => self.k.hardsigmoid,
            Activation::Sigmoid => self.k.sigmoid,
        };
        let args = Args::default().ptr(x).u32(x.len());
        self.launch(f, (blocks_1d(x.len()), 1, 1), (BLOCK_1D, 1, 1), args)
    }

    /// 原地加偏置：`x` 是 `[C, hw]`，`x[c, i] += bias[c]`。
    pub fn add_bias_device(
        &self,
        x: &mut DeviceBuffer<'_>,
        bias: &DeviceBuffer<'_>,
        hw: usize,
    ) -> Result<(), String> {
        assert_eq!(x.len(), bias.len() * hw, "偏置长度与张量形状不符");
        let args = Args::default()
            .ptr(x)
            .ptr(bias)
            .u32(hw)
            .u32(bias.len())
            .u32(x.len());
        self.launch(
            self.k.add_bias,
            (blocks_1d(x.len()), 1, 1),
            (BLOCK_1D, 1, 1),
            args,
        )
    }

    /// 矩阵乘（同 `pagesift_infer::ops::matmul`）：`a` 是 `[M, K]`，`b` 是 `[K, N]`，返回 `[M, N]` 行优先。
    pub fn matmul(&self, a: &Tensor, b: &Tensor) -> Result<Vec<f32>, String> {
        let (m, k) = a.tc();
        let (kb, n) = b.tc();
        assert_eq!(k, kb, "矩阵乘的内维不同");
        let (da, db) = (self.upload(&a.data)?, self.upload(&b.data)?);
        let mut dc = self.alloc(m * n)?;
        self.gemm_device(&da, &db, &mut dc, m, n, k)?;
        dc.to_vec()
    }

    /// 二维卷积（同 `pagesift_infer::ops::conv2d`）：`x` 是 `[C, H, W]`，`w` 是 `[O, C/groups, kh, kw]`；
    /// 返回 `[O, OH, OW]` 行优先的数据。
    #[allow(clippy::too_many_arguments)]
    pub fn conv2d(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&[f32]>,
        stride: (usize, usize),
        pad: (usize, usize),
        dilation: (usize, usize),
        groups: usize,
    ) -> Result<Vec<f32>, String> {
        let (c, h, wd) = x.chw();
        // [协议] 卷积权重是 4 维 [O, C/groups, kh, kw]（PyTorch Conv2d.weight 的布局）
        assert_eq!(w.shape.len(), 4, "卷积权重要 [O, C/groups, kh, kw]");
        let s = ConvShape {
            c,
            h,
            w: wd,
            o: w.shape[0],
            kh: w.shape[2],
            kw: w.shape[3],
            stride,
            pad,
            dilation,
            groups,
        };
        assert_eq!(
            c,
            w.shape[1] * groups,
            "卷积输入通道 {c} 与权重 {}×{groups} 不符",
            w.shape[1]
        );
        let dx = self.upload(&x.data)?;
        let dw = self.upload(&w.data)?;
        let db = bias.map(|b| self.upload(b)).transpose()?;
        self.conv2d_device(&dx, &dw, db.as_ref(), &s)?.to_vec()
    }

    /// 原地激活一段主机内存（同 `Activation::apply_all`）。
    pub fn activate(&self, x: &mut [f32], act: Activation) -> Result<(), String> {
        if act == Activation::Identity {
            return Ok(());
        }
        let mut d = self.upload(x)?;
        self.activate_device(&mut d, act)?;
        d.copy_to_host(x)
    }
}
