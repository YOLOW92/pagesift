//! 模型配置（config.json）里推理要用的字段，缺省值取 transformers `PPDocLayoutV3Config` 的类属性。
//! 只支持 PP-DocLayoutV3 发布权重用到的那种结构，遇到别的取值直接报错，不默默算错。

use pagesift_infer::Activation;

/// 推理要用的配置。
#[derive(Debug, Clone)]
pub struct Config {
    pub d_model: usize,
    pub encoder_heads: usize,
    pub decoder_heads: usize,
    pub decoder_points: usize,
    pub num_queries: usize,
    /// 编码器、解码器输入各层特征的步长（相对网络输入）。
    pub feat_strides: Vec<usize>,
    /// 做 AIFI（层内自注意力）的那一层在 `feat_strides` 里的下标。
    pub aifi_level: usize,
    /// CSP 块、下采样卷积等的激活（`activation_function`）。
    pub act: Activation,
    pub encoder_act: Activation,
    pub decoder_act: Activation,
    pub layer_norm_eps: f32,
    pub pos_temperature: f64,
    pub pointer_dim: usize,
    /// 类别号 → 名字（`id2label`）。
    pub labels: Vec<String>,
}

/// [协议] `PPDocLayoutV3Config.feat_strides` 的缺省值
const DEFAULT_STRIDES: [usize; 3] = [8, 16, 32];
/// [协议] `encoder_attention_heads`、`decoder_attention_heads` 的缺省值
const DEFAULT_HEADS: usize = 8;
/// [协议] `decoder_n_points` 的缺省值
const DEFAULT_POINTS: usize = 4;
/// [协议] `num_queries` 的缺省值
const DEFAULT_QUERIES: usize = 300;

fn activation(name: &str) -> Result<Activation, String> {
    match name {
        "relu" => Ok(Activation::Relu),
        "silu" | "swish" => Ok(Activation::Silu),
        "gelu" => Ok(Activation::Gelu),
        _ => Err(format!("unsupported activation {name}")),
    }
}

impl Config {
    pub fn from_json(v: &serde_json::Value) -> Result<Config, String> {
        let int = |key: &str, def: usize| -> Result<usize, String> {
            match &v[key] {
                serde_json::Value::Null => Ok(def),
                x => x
                    .as_u64()
                    .map(|n| n as usize)
                    .ok_or(format!("{key} must be an integer")),
            }
        };
        let list = |key: &str, def: &[usize]| -> Result<Vec<usize>, String> {
            match v[key].as_array() {
                None => Ok(def.to_vec()),
                Some(a) => a
                    .iter()
                    .map(|x| {
                        x.as_u64()
                            .map(|n| n as usize)
                            .ok_or(format!("{key} must be an integer"))
                    })
                    .collect(),
            }
        };
        let act = |key: &str, def: &str| activation(v[key].as_str().unwrap_or(def));
        let flag = |key: &str, def: bool| v[key].as_bool().unwrap_or(def);

        // 只实现发布权重的结构：后归一化、查询取自编码器输出、掩码框作初始参考框、CSP 不扩通道
        if flag("normalize_before", false) {
            return Err("normalize_before is not supported".into());
        }
        if flag("learn_initial_query", false) {
            return Err("learn_initial_query is not supported".into());
        }
        if !flag("mask_enhanced", true) {
            return Err(
                "only mask_enhanced is supported (mask boxes as the initial reference boxes)"
                    .into(),
            );
        }
        if v["hidden_expansion"].as_f64().unwrap_or(1.0) != 1.0 {
            return Err("only hidden_expansion = 1 is supported".into());
        }

        // [协议] 缺省值取 PPDocLayoutV3Config：d_model 256、注意力头 8、采样点 4、查询 300、
        // 步长 (8, 16, 32)、AIFI 在第 2 层、指针头宽 64、位置编码温度 10000
        let d_model = int("d_model", 256)?;
        let feat_strides = match v["feat_strides"].as_array() {
            Some(_) => list("feat_strides", &DEFAULT_STRIDES)?,
            None => list("feature_strides", &DEFAULT_STRIDES)?,
        };
        let proj = list("encode_proj_layers", &[2])?;
        if proj.len() != 1 || int("encoder_layers", 1)? != 1 {
            return Err("only one AIFI layer is supported".into());
        }
        let levels = int("num_feature_levels", feat_strides.len())?;
        if levels != feat_strides.len() || int("encoder_hidden_dim", d_model)? != d_model {
            return Err("only decoder layers that take encoder outputs directly are supported (same layer count and channels)".into());
        }
        let labels = parse_labels(&v["id2label"])?;
        Ok(Config {
            d_model,
            encoder_heads: int("encoder_attention_heads", DEFAULT_HEADS)?,
            decoder_heads: int("decoder_attention_heads", DEFAULT_HEADS)?,
            decoder_points: int("decoder_n_points", DEFAULT_POINTS)?,
            num_queries: int("num_queries", DEFAULT_QUERIES)?,
            aifi_level: proj[0],
            feat_strides,
            act: act("activation_function", "silu")?,
            encoder_act: act("encoder_activation_function", "gelu")?,
            decoder_act: act("decoder_activation_function", "relu")?,
            // [协议] PPDocLayoutV3Config 的 layer_norm_eps 缺省 1e-5
            layer_norm_eps: v["layer_norm_eps"].as_f64().unwrap_or(1e-5) as f32,
            pos_temperature: v["positional_encoding_temperature"]
                .as_f64()
                .unwrap_or(10000.0),
            pointer_dim: int("global_pointer_head_size", 64)?,
            labels,
        })
    }
}

/// `id2label`：键是类别号的字符串。
fn parse_labels(v: &serde_json::Value) -> Result<Vec<String>, String> {
    let obj = v.as_object().ok_or("config.json has no id2label")?;
    let mut labels = vec![String::new(); obj.len()];
    for (k, name) in obj {
        let i: usize = k
            .parse()
            .map_err(|_| format!("id2label key {k} is not an integer"))?;
        let slot = labels
            .get_mut(i)
            .ok_or(format!("id2label class ids are not contiguous at {i}"))?;
        *slot = name
            .as_str()
            .ok_or("id2label values must be strings")?
            .to_string();
    }
    Ok(labels)
}

/// inference.yml 里的 `label_list`（PaddleX 的细分类名，例如把 `formula` 分成 `display_formula`、
/// `inline_formula`）。只认 `label_list:` 下一行一个 `- 名字` 的写法；没有或格式不对时返回空。
pub fn parse_label_list(yml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in yml.lines() {
        if line.trim_end() == "label_list:" {
            inside = true;
            continue;
        }
        if !inside {
            continue;
        }
        match line.trim_start().strip_prefix("- ") {
            // 列表项与键同级或更深缩进；遇到下一个键就结束
            Some(name) => out.push(name.trim().to_string()),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 读细分类名() {
        let yml = "arch: DETR\nlabel_list:\n- abstract\n- display_formula\nHpi:\n  x: 1\n";
        assert_eq!(parse_label_list(yml), ["abstract", "display_formula"]);
        assert!(parse_label_list("arch: DETR\n").is_empty());
    }

    #[test]
    fn 读配置() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"d_model": 256, "feature_strides": [8, 16, 32], "id2label": {"1": "b", "0": "a"},
                "encoder_activation_function": "gelu"}"#,
        )
        .unwrap();
        let c = Config::from_json(&v).unwrap();
        assert_eq!(c.labels, ["a", "b"]);
        assert_eq!(c.feat_strides, [8, 16, 32]);
        assert_eq!(c.aifi_level, 2);
        assert_eq!(c.encoder_act, Activation::Gelu);
    }
}
