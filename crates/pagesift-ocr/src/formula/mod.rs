//! 公式识别（PP-FormulaNet_plus-L）的文本一侧：mBART 式解码器的贪心生成、字节级 BPE 的解码、
//! 把输出整理成 LaTeX 的后处理。结构照 transformers 的 `modeling_pp_formulanet.py` 与
//! `processing_pp_formulanet.py`（Apache-2.0，PaddlePaddle 与 HuggingFace）。
//!
//! 流程：公式图 → 图像编码器得到 memory `[L, d_model]` → [`Decoder::generate`] 得到 token →
//! [`Tokenizer::decode`] 得到原始文本 → [`to_latex`] 去掉多余空格得到 LaTeX。
//!
//! 图像编码器不在这里。

mod attention;
mod decoder;
mod latex;
mod tokenizer;

pub use decoder::Decoder;
pub use latex::to_latex;
pub use tokenizer::Tokenizer;
