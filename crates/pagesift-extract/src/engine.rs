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
    pub base_raw: String,
    base_fields: Vec<CField<S>>,
    fields: Vec<CField<S>>,
}

/// Extracted leaf trace before selector-method attribution (CSS vs XPath vs Adaptive).
#[derive(Debug, Clone)]
pub(crate) struct EngineTrace {
    pub field_path: String,
    pub field_name: String,
    pub value: Value,
    pub raw_quote: Option<String>,
    pub attr_name: Option<String>,
    pub raw_selector: Option<String>,
}

trait TraceSink {
    const ENABLED: bool;
    fn push(&mut self, trace: EngineTrace);
}

struct NoTrace;

impl TraceSink for NoTrace {
    const ENABLED: bool = false;
    #[inline(always)]
    fn push(&mut self, _: EngineTrace) {}
}

impl TraceSink for Vec<EngineTrace> {
    const ENABLED: bool = true;
    fn push(&mut self, trace: EngineTrace) {
        Vec::push(self, trace);
    }
}

struct CField<S> {
    name: String,
    path: String,
    sel: Option<S>,
    raw_sel: Option<String>,
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
            base_raw: schema.base_selector.trim().to_owned(),
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
    let raw_sel = f.selector.as_ref().map(|s| s.trim().to_owned());
    Ok(CField {
        name: f.name.clone(),
        path: path.to_owned(),
        sel,
        raw_sel,
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
    run_with_sink(b, c, &mut NoTrace)
}

pub(crate) fn run_with_evidence<B: Backend>(
    b: &B,
    c: &Compiled<B::Sel>,
) -> Result<(Vec<Value>, Vec<EngineTrace>), Error> {
    let mut traces = Vec::new();
    let items = run_with_sink(b, c, &mut traces)?;
    Ok((items, traces))
}

fn run_with_sink<B: Backend, T: TraceSink>(
    b: &B,
    c: &Compiled<B::Sel>,
    sink: &mut T,
) -> Result<Vec<Value>, Error> {
    let mut out = Vec::new();
    for el in b.select_base(&c.base)? {
        let mut item = Map::new();
        let prefix = if T::ENABLED {
            format!("/{}", out.len())
        } else {
            String::new()
        };
        if T::ENABLED {
            let mut item_traces = Vec::new();
            extract_into(
                b,
                &el,
                &c.base_fields,
                &mut item,
                &c.base_raw,
                &prefix,
                &mut item_traces,
            )?;
            extract_into(
                b,
                &el,
                &c.fields,
                &mut item,
                &c.base_raw,
                &prefix,
                &mut item_traces,
            )?;
            if !item.is_empty() {
                out.push(Value::Object(item));
                for t in item_traces {
                    sink.push(t);
                }
            }
        } else {
            extract_into(b, &el, &c.base_fields, &mut item, &c.base_raw, "", sink)?;
            extract_into(b, &el, &c.fields, &mut item, &c.base_raw, "", sink)?;
            if !item.is_empty() {
                out.push(Value::Object(item));
            }
        }
    }
    Ok(out)
}

fn extract_into<B: Backend, T: TraceSink>(
    b: &B,
    el: &B::Item,
    fields: &[CField<B::Sel>],
    item: &mut Map<String, Value>,
    base_raw: &str,
    prefix: &str,
    sink: &mut T,
) -> Result<(), Error> {
    for f in fields {
        let pointer = if T::ENABLED {
            format!("{prefix}/{}", f.name)
        } else {
            String::new()
        };
        // 和 Crawl4AI 一样，值为 None（包括 default: null）的字段不出现在结果里
        if let Some(v) = extract_field(b, el, f, base_raw, &pointer, sink)?
            && !v.is_null()
        {
            item.insert(f.name.clone(), v);
        }
    }
    Ok(())
}

fn push_default<T: TraceSink>(sink: &mut T, f: &CField<impl Sized>, base_raw: &str, pointer: &str) {
    if T::ENABLED
        && let Some(val) = f.default.as_ref().filter(|v| !v.is_null())
    {
        sink.push(EngineTrace {
            field_path: pointer.to_owned(),
            field_name: f.name.clone(),
            value: val.clone(),
            raw_quote: None,
            attr_name: None,
            raw_selector: f.raw_sel.clone().or_else(|| Some(base_raw.to_owned())),
        });
    }
}

fn extract_field<B: Backend, T: TraceSink>(
    b: &B,
    el: &B::Item,
    f: &CField<B::Sel>,
    base_raw: &str,
    pointer: &str,
    sink: &mut T,
) -> Result<Option<Value>, Error> {
    let sibling;
    let el = match &f.source {
        Some(src) => match b.sibling(el, src, &f.path)? {
            Some(s) => {
                sibling = s;
                &sibling
            }
            None => {
                push_default(sink, f, base_raw, pointer);
                return Ok(f.default.clone());
            }
        },
        None => el,
    };
    match &f.kind {
        CKind::Value(steps) => single_value(b, el, f, steps, base_raw, pointer, sink),
        CKind::Computed => {
            push_default(sink, f, base_raw, pointer);
            Ok(f.default.clone())
        }
        CKind::Nested(sub) => {
            let sel = f
                .sel
                .as_ref()
                .expect("nested fields always have a selector");
            let mut obj = Map::new();
            if let Some(n) = b.select(el, sel, &f.path, true)?.first() {
                extract_into(
                    b,
                    n,
                    sub,
                    &mut obj,
                    f.raw_sel.as_deref().unwrap_or(base_raw),
                    pointer,
                    sink,
                )?;
            }
            Ok(Some(Value::Object(obj)))
        }
        CKind::List(sub) => {
            let sel = f.sel.as_ref().expect("list fields always have a selector");
            let mut arr = Vec::new();
            for n in b.select(el, sel, &f.path, false)? {
                let mut obj = Map::new();
                let elem_ptr = if T::ENABLED {
                    format!("{pointer}/{}", arr.len())
                } else {
                    String::new()
                };
                extract_into(
                    b,
                    &n,
                    sub,
                    &mut obj,
                    f.raw_sel.as_deref().unwrap_or(base_raw),
                    &elem_ptr,
                    sink,
                )?;
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

fn single_value<B: Backend, T: TraceSink>(
    b: &B,
    el: &B::Item,
    f: &CField<B::Sel>,
    steps: &[CStep],
    base_raw: &str,
    pointer: &str,
    sink: &mut T,
) -> Result<Option<Value>, Error> {
    let target = match &f.sel {
        Some(sel) => match b.select(el, sel, &f.path, true)?.into_iter().next() {
            Some(t) => t,
            None => {
                push_default(sink, f, base_raw, pointer);
                return Ok(f.default.clone());
            }
        },
        None => el.clone(),
    };
    let mut raw_quote: Option<String> = None;
    let mut attr_name: Option<String> = None;
    let mut cur = Some(Cur::Item(target));
    for step in steps {
        let Some(c) = cur.take() else { break };
        cur = match (step, Some(c)) {
            (_, None) => None,
            (CStep::Text, Some(Cur::Item(i))) => {
                let txt = b.text(&i);
                if T::ENABLED && raw_quote.is_none() {
                    raw_quote = Some(txt.clone());
                }
                Some(Cur::Str(txt))
            }
            (CStep::Html, Some(Cur::Item(i))) => {
                if T::ENABLED && raw_quote.is_none() {
                    raw_quote = Some(b.text(&i));
                }
                Some(Cur::Str(b.html(&i)))
            }
            (CStep::Text | CStep::Html, Some(s)) => Some(s),
            (CStep::Attr(a), Some(Cur::Item(i))) => {
                let attr_val = b.attr(&i, a);
                if T::ENABLED && raw_quote.is_none() {
                    attr_name = Some(a.clone());
                    raw_quote = attr_val.clone();
                }
                attr_val.map(Cur::Str)
            }
            (CStep::Attr(_), Some(Cur::Str(_))) => None,
            (CStep::Regex(re, group), Some(c)) => {
                let s = match c {
                    Cur::Item(i) => {
                        let txt = b.text(&i);
                        if T::ENABLED && raw_quote.is_none() {
                            raw_quote = Some(txt.clone());
                        }
                        txt
                    }
                    Cur::Str(s) => s,
                };
                re.captures(&s)
                    .and_then(|caps| caps.get(*group))
                    .map(|m| Cur::Str(m.as_str().to_owned()))
            }
        };
    }
    let matched_value = match cur {
        Some(Cur::Str(s)) => Some(match f.transform {
            Some(t) => t.apply(&s),
            None => s,
        }),
        // 流水线没有产生字符串（例如只有选择器没有步骤），按文本处理
        Some(Cur::Item(i)) => {
            let txt = b.text(&i);
            if T::ENABLED && raw_quote.is_none() {
                raw_quote = Some(txt.clone());
            }
            Some(txt)
        }
        None => None,
    };
    let final_val = matched_value
        .as_ref()
        .map(|s| Value::String(s.clone()))
        .or_else(|| f.default.clone());
    if T::ENABLED
        && let Some(val) = final_val.as_ref().filter(|v| !v.is_null())
    {
        let came_from_match = matched_value.is_some();
        sink.push(EngineTrace {
            field_path: pointer.to_owned(),
            field_name: f.name.clone(),
            value: val.clone(),
            raw_quote: if came_from_match { raw_quote } else { None },
            attr_name: if came_from_match { attr_name } else { None },
            raw_selector: f.raw_sel.clone().or_else(|| Some(base_raw.to_owned())),
        });
    }
    Ok(final_val)
}
