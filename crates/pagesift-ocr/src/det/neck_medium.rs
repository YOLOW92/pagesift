//! medium 检测模型的颈部（LK-PAN + 类内关系块）。结构照 transformers 的
//! `modeling_pp_ocrv6_medium_det.py`（Apache-2.0，PaddlePaddle 与 HuggingFace）：
//! 1×1 统一通道 → 自顶向下相加 → 9×9 投影 → 自底向上相加 → 9×9 横向 → 每段一个类内关系块 →
//! 上采样到步长 4 → 按 p5、p4、p3、p2 拼接。头和 small 相同（[`super::neck::Head`]）。

use pagesift_infer::exec::Exec;
use pagesift_infer::{Activation, BN_EPS, BatchNorm, Conv2d, Weights};

use super::neck::upsample_on;

/// 读一个步长 1、补边 (kh/2, kw/2)、groups = 1 的卷积。
fn conv_same(w: &Weights, p: &str) -> Result<Conv2d, String> {
    let shape = &w.get(&format!("{p}.weight"))?.shape;
    let pad = (shape[2] / 2, shape[3] / 2);
    Conv2d::load(w, p, (1, 1), pad, (1, 1), 1)
}

/// [协议] 类内关系块的结构：长、中、短三组，每组对称、纵向、横向三个卷积（`PPOCRV6MediumDetIntraclassBlock`）
type Triple<T> = [T; 3];

/// 类内关系块：三组（长、中、短）“对称 + 纵向 + 横向”卷积相加，残差接回。
#[derive(Debug, Clone)]
struct IntraclassBlock {
    reduce: Conv2d,
    /// 每组是 [对称, 纵向, 横向]，顺序同参考实现的相加顺序。
    groups: Triple<Triple<Conv2d>>,
    /// 1×1 卷积 + 批归一化（已并入）+ ReLU。
    fin: Conv2d,
}

impl IntraclassBlock {
    fn load(w: &Weights, p: &str) -> Result<IntraclassBlock, String> {
        let group = |ratio: &str| -> Result<Triple<Conv2d>, String> {
            Ok([
                conv_same(w, &format!("{p}.symmetric_conv_long_{ratio}"))?,
                conv_same(w, &format!("{p}.vertical_long_to_small_conv_{ratio}"))?,
                conv_same(w, &format!("{p}.horizontal_small_to_long_conv_{ratio}"))?,
            ])
        };
        let mut fin = conv_same(w, &format!("{p}.conv_final.convolution"))?;
        fin.fold(&BatchNorm::load(
            w,
            &format!("{p}.conv_final.norm"),
            BN_EPS,
        )?);
        Ok(IntraclassBlock {
            reduce: conv_same(w, &format!("{p}.conv_reduce_channel"))?,
            groups: [
                group("longratio")?,
                group("midratio")?,
                group("shortratio")?,
            ],
            fin,
        })
    }

    fn forward_on<E: Exec>(&self, e: &E, x: &E::T) -> E::T {
        let mut h = e.conv(&self.reduce, x);
        for [sym, ver, hor] in &self.groups {
            let mut y = e.conv(sym, &h);
            e.add_assign(&mut y, &e.conv(ver, &h));
            e.add_assign(&mut y, &e.conv(hor, &h));
            h = y;
        }
        let mut y = e.conv(&self.fin, &h);
        e.act(Activation::Relu, &mut y);
        e.add_assign(&mut y, x);
        y
    }
}

/// medium 颈部。
#[derive(Debug, Clone)]
pub struct MediumNeck {
    adjust: Vec<Conv2d>,
    project: Vec<Conv2d>,
    /// 自底向上的下采样卷积，比段数少一个。
    pan_head: Vec<Conv2d>,
    lateral: Vec<Conv2d>,
    intraclass: Vec<IntraclassBlock>,
}

impl MediumNeck {
    /// `p` 是权重名前缀（`model.neck`）。段数从权重名数出来。
    pub fn load(w: &Weights, p: &str) -> Result<MediumNeck, String> {
        let mut n = MediumNeck {
            adjust: Vec::new(),
            project: Vec::new(),
            pan_head: Vec::new(),
            lateral: Vec::new(),
            intraclass: Vec::new(),
        };
        let mut i = 0;
        while w
            .opt(&format!(
                "{p}.input_channel_adjustment_convolution.{i}.weight"
            ))
            .is_some()
        {
            n.adjust.push(conv_same(
                w,
                &format!("{p}.input_channel_adjustment_convolution.{i}"),
            )?);
            n.project.push(conv_same(
                w,
                &format!("{p}.input_feature_projection_convolution.{i}"),
            )?);
            n.lateral.push(conv_same(
                w,
                &format!("{p}.path_aggregation_lateral_convolution.{i}"),
            )?);
            n.intraclass.push(IntraclassBlock::load(
                w,
                &format!("{p}.intraclass_blocks.{i}"),
            )?);
            if i > 0 {
                // [协议] pan_head：3×3、补边 1、步长 2（`PPOCRV6MediumDetNeck.__init__`）
                n.pan_head.push(Conv2d::load(
                    w,
                    &format!("{p}.path_aggregation_head_convolution.{}", i - 1),
                    (2, 2),
                    (1, 1),
                    (1, 1),
                    1,
                )?);
            }
            i += 1;
        }
        if i == 0 {
            return Err("no medium neck in the weights".into());
        }
        Ok(n)
    }

    /// 输入骨干各段特征（步长 4、8、16、32），输出步长 4 的融合特征。
    pub fn forward_on<E: Exec>(&self, e: &E, feats: &[E::T]) -> Result<E::T, String> {
        let n = self.adjust.len();
        if feats.len() != n {
            return Err(format!(
                "the backbone gave {} feature stages, the neck needs {n}",
                feats.len()
            ));
        }
        let mut td: Vec<E::T> = self
            .adjust
            .iter()
            .zip(feats)
            .map(|(c, f)| e.conv(c, f))
            .collect();
        for i in (0..n - 1).rev() {
            let up = upsample_on(e, &td[i + 1], 2);
            if e.shape(&up) != e.shape(&td[i]) {
                return Err(format!(
                    "neck upsampling size mismatch: {:?} and {:?}",
                    e.shape(&up),
                    e.shape(&td[i])
                ));
            }
            e.add_assign(&mut td[i], &up);
        }
        let mut bu: Vec<E::T> = self
            .project
            .iter()
            .zip(&td)
            .map(|(c, f)| e.conv(c, f))
            .collect();
        drop(td);
        for i in 1..n {
            let (lo, hi) = bu.split_at_mut(i);
            e.add_assign(&mut hi[0], &e.conv(&self.pan_head[i - 1], &lo[i - 1]));
        }
        let refined: Vec<E::T> = (0..n)
            .map(|i| {
                let lateral = e.conv(&self.lateral[i], &bu[i]);
                let block = self.intraclass[i].forward_on(e, &lateral);
                upsample_on(e, &block, 1 << i)
            })
            .collect();
        // [协议] 按 [p5, p4, p3, p2] 的顺序拼通道（`upsampled[::-1]`）
        let refs: Vec<&E::T> = refined.iter().rev().collect();
        Ok(e.concat(&refs))
    }
}
