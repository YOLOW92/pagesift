//! ES6 模板字符串（反引号 `` `/api/${endpoint}` ``）解析与泛化还原。

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart {
    /// 静态字面量片段
    Literal(String),
    /// `${...}` 表达式中的标识符或变量名
    Expr(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateLiteral {
    pub parts: Vec<TemplatePart>,
}

impl TemplateLiteral {
    /// 尝试根据常量/参数绑定表计算出具体字符串。
    pub fn try_eval(&self, bindings: &HashMap<&str, String>) -> Option<String> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                TemplatePart::Literal(s) => out.push_str(s),
                TemplatePart::Expr(var) => {
                    let val = bindings.get(var.as_str())?;
                    out.push_str(val);
                }
            }
        }
        Some(out)
    }

    /// 将未绑定的动态参数泛化为 REST 路径模板（如 `/api/{resource}/{id}`）。
    pub fn to_generalized_path(&self, bindings: &HashMap<&str, String>) -> String {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                TemplatePart::Literal(s) => out.push_str(s),
                TemplatePart::Expr(var) => {
                    if let Some(val) = bindings.get(var.as_str()) {
                        out.push_str(val);
                    } else {
                        out.push('{');
                        out.push_str(var);
                        out.push('}');
                    }
                }
            }
        }
        out
    }

    /// 判断模板是否全部为静态内容（无动态插值）。
    pub fn is_static(&self) -> bool {
        self.parts
            .iter()
            .all(|p| matches!(p, TemplatePart::Literal(_)))
    }
}
