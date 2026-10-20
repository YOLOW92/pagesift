//! CSS 和 XPath 共用的 schema 执行逻辑，语义按 Crawl4AI 的 `JsonElementExtractionStrategy`。

use regex::Regex;
use serde_json::{Map, Value};

use crate::Error;
use crate::schema::{Field, FieldKind, Schema, Step, Transform};

/// 选择器出现的位置，XPath 对三种位置的写法处理不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Role {
    Base,
    Field,
    Source,
}

/// 一种选择器引擎在一份文档上的视图。
pub(crate) trait Backend {
    type Item: Clone;
    type Sel;
    fn select_base(&self, sel: &Self::Sel) -> Result<Vec<Self::Item>, Error>;
    fn select(
        &self,
        ctx: &Self::Item,
        sel: &Self::Sel,
        path: &str,
        first_only: bool,
    ) -> Result<Vec<Self::Item>, Error>;
    fn sibling(
        &self,
        ctx: &Self::Item,
        sel: &Self::Sel,
        path: &str,
    ) -> Result<Option<Self::Item>, Error>;
    fn text(&self, item: &Self::Item) -> String;
    fn html(&self, item: &Self::Item) -> String;
    fn attr(&self, item: &Self::Item, name: &str) -> Option<String>;
}

pub(crate) struct Compiled<S> {
    pub base: S,
    base_fields: Vec<CField<S>>,
    fields: Vec<CField<S>>,
}

struct CField<S> {
    name: String,
    path: String,
    sel: Option<S>,
    source: Option<S>,
    kind: CKind<S>,
    default: Option<Value>,
    transform: Option<Transform>,
}

enum CKind<S> {
    Value(Vec<CStep>),
    Nested(Vec<CField<S>>),
    List(Vec<CField<S>>),
    Computed,
}

enum CStep {
    Text,
    Attr(String),
    Html,
    Regex(Regex, usize),
}

type SelCompiler<'a, S> = dyn FnMut(&str, &str, Role) -> Result<S, Error> + 'a;

impl<S> Compiled<S> {
    pub fn compile(schema: &Schema, compile: &mut SelCompiler<'_, S>) -> Result<Self, Error> {
        Ok(Compiled {
            base: compile(&schema.base_selector, "baseSelector", Role::Base)?,
            base_fields: compile_fields(&schema.base_fields, "baseFields", compile)?,
            fields: compile_fields(&schema.fields, "fields", compile)?,
        })
    }
}

fn compile_fields<S>(
    fields: &[Field],
    path: &str,
    compile: &mut SelCompiler<'_, S>,
) -> Result<Vec<CField<S>>, Error> {
    fields
        .iter()
        .enumerate()
        .map(|(i, f)| compile_field(f, &format!("{path}[{i}]"), compile))
        .collect()
}

fn compile_field<S>(
    f: &Field,
    path: &str,
    compile: &mut SelCompiler<'_, S>,
) -> Result<CField<S>, Error> {
    let sel = match &f.selector {
        Some(s) => Some(compile(s, &format!("{path}.selector"), Role::Field)?),
        None => None,
    };
    let source = match &f.source {
        Some(s) => Some(compile(s, &format!("{path}.source"), Role::Source)?),
        None => None,
    };
    let sub = |fs: &[Field], compile: &mut SelCompiler<'_, S>| {
        compile_fields(fs, &format!("{path}.fields"), compile)
    };
    let needs_selector = matches!(
        f.kind,
        FieldKind::Nested(_) | FieldKind::List(_) | FieldKind::NestedList(_)
    );
    if needs_selector && sel.is_none() {
        return Err(Error::Schema {
            path: format!("{path}.selector"),
            message: "is required for nested, list and nested_list fields".into(),
        });
    }
    if matches!(&f.kind, FieldKind::Value(steps) if steps.is_empty()) {
        return Err(Error::Schema {
            path: format!("{path}.type"),
            message: "type pipeline must not be empty".into(),
        });
    }
    let kind = match &f.kind {
        FieldKind::Value(steps) => CKind::Value(
            steps
                .iter()
                .map(|s| compile_step(s, path))
                .collect::<Result<_, _>>()?,
        ),
        FieldKind::Nested(fs) => CKind::Nested(sub(fs, compile)?),
        FieldKind::List(fs) | FieldKind::NestedList(fs) => CKind::List(sub(fs, compile)?),
        FieldKind::Computed => CKind::Computed,
    };
    Ok(CField {
        name: f.name.clone(),
        path: path.to_owned(),
        sel,
        source,
        kind,
        default: f.default.clone(),
        transform: f.transform,
    })
}

fn compile_step(step: &Step, path: &str) -> Result<CStep, Error> {
    Ok(match step {
        Step::Text => CStep::Text,
        Step::Html => CStep::Html,
        Step::Attribute(a) => CStep::Attr(a.clone()),
        Step::Regex { pattern, group } => {
            let re = Regex::new(pattern).map_err(|e| Error::Regex {
                path: format!("{path}.pattern"),
                message: format!("{e} (note: look-around and backreferences are not supported)"),
            })?;
            if *group >= re.captures_len() {
                return Err(Error::Regex {
                    path: format!("{path}.group"),
                    message: format!(
                        "group {group} does not exist; the pattern has {} capture group(s)",
                        re.captures_len() - 1
                    ),
                });
            }
            CStep::Regex(re, *group)
        }
    })
}

pub(crate) fn run<B: Backend>(b: &B, c: &Compiled<B::Sel>) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for el in b.select_base(&c.base)? {
        let mut item = Map::new();
        extract_into(b, &el, &c.base_fields, &mut item)?;
        extract_into(b, &el, &c.fields, &mut item)?;
        if !item.is_empty() {
            out.push(Value::Object(item));
        }
    }
    Ok(out)
}

fn extract_into<B: Backend>(
    b: &B,
    el: &B::Item,
    fields: &[CField<B::Sel>],
    item: &mut Map<String, Value>,
) -> Result<(), Error> {
    for f in fields {
        // 和 Crawl4AI 一样，值为 None（包括 default: null）的字段不出现在结果里
        if let Some(v) = extract_field(b, el, f)?
            && !v.is_null()
        {
            item.insert(f.name.clone(), v);
        }
    }
    Ok(())
}

fn extract_field<B: Backend>(
    b: &B,
    el: &B::Item,
    f: &CField<B::Sel>,
) -> Result<Option<Value>, Error> {
    let sibling;
    let el = match &f.source {
        Some(src) => match b.sibling(el, src, &f.path)? {
            Some(s) => {
                sibling = s;
                &sibling
            }
            None => return Ok(f.default.clone()),
        },
        None => el,
    };
    match &f.kind {
        CKind::Value(steps) => single_value(b, el, f, steps),
        CKind::Computed => Ok(f.default.clone()),
        CKind::Nested(sub) => {
            let sel = f
                .sel
                .as_ref()
                .expect("nested fields always have a selector");
            let mut obj = Map::new();
            if let Some(n) = b.select(el, sel, &f.path, true)?.first() {
                extract_into(b, n, sub, &mut obj)?;
            }
            Ok(Some(Value::Object(obj)))
        }
        CKind::List(sub) => {
            let sel = f.sel.as_ref().expect("list fields always have a selector");
            let mut arr = Vec::new();
            for n in b.select(el, sel, &f.path, false)? {
                let mut obj = Map::new();
                extract_into(b, &n, sub, &mut obj)?;
                arr.push(Value::Object(obj));
            }
            Ok(Some(Value::Array(arr)))
        }
    }
}

enum Cur<I> {
    Item(I),
    Str(String),
}

fn single_value<B: Backend>(
    b: &B,
    el: &B::Item,
    f: &CField<B::Sel>,
    steps: &[CStep],
) -> Result<Option<Value>, Error> {
    let target = match &f.sel {
        Some(sel) => match b.select(el, sel, &f.path, true)?.into_iter().next() {
            Some(t) => t,
            None => return Ok(f.default.clone()),
        },
        None => el.clone(),
    };
    let mut cur = Some(Cur::Item(target));
    for step in steps {
        let Some(c) = cur.take() else { break };
        cur = match (step, Some(c)) {
            (_, None) => None,
            (CStep::Text, Some(Cur::Item(i))) => Some(Cur::Str(b.text(&i))),
            (CStep::Html, Some(Cur::Item(i))) => Some(Cur::Str(b.html(&i))),
            (CStep::Text | CStep::Html, Some(s)) => Some(s),
            (CStep::Attr(a), Some(Cur::Item(i))) => b.attr(&i, a).map(Cur::Str),
            (CStep::Attr(_), Some(Cur::Str(_))) => None,
            (CStep::Regex(re, group), Some(c)) => {
                let s = match c {
                    Cur::Item(i) => b.text(&i),
                    Cur::Str(s) => s,
                };
                re.captures(&s)
                    .and_then(|caps| caps.get(*group))
                    .map(|m| Cur::Str(m.as_str().to_owned()))
            }
        };
    }
    let value = match cur {
        Some(Cur::Str(s)) => Some(match f.transform {
            Some(t) => t.apply(&s),
            None => s,
        }),
        // 流水线没有产生字符串（例如只有选择器没有步骤），按文本处理
        Some(Cur::Item(i)) => Some(b.text(&i)),
        None => None,
    };
    Ok(value.map(Value::String).or_else(|| f.default.clone()))
}
