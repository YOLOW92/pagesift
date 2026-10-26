//! html5lib 树构建测试的 `.dat` 格式（见数据目录里的 README.md）。

pub struct Case {
    /// 在文件里的序号（从 0 开始），和别的实现的报告对得上。
    pub index: usize,
    pub data: String,
    pub fragment: Option<String>,
    /// `Some(true)` = `#script-on`，`Some(false)` = `#script-off`，`None` 两种模式都适用。
    pub scripting: Option<bool>,
    pub document: String,
}

pub fn parse(text: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let mut section = "";
    let mut cur: Option<Case> = None;
    for line in text.split('\n') {
        // 新用例从 `#data` 开始（`#document` 段里的行都以 `| ` 开头或是多行文本的续行）
        let starts_case = line == "#data" && matches!(section, "" | "#document" | "#description");
        if starts_case {
            if let Some(c) = cur.take() {
                cases.push(finish(c));
            }
            cur = Some(Case {
                index: cases.len(),
                data: String::new(),
                fragment: None,
                scripting: None,
                document: String::new(),
            });
            section = "#data";
            continue;
        }
        if line == "#description" && (section.is_empty() || section == "#document") {
            if let Some(c) = cur.take() {
                cases.push(finish(c));
            }
            section = "#description";
            continue;
        }
        let Some(c) = cur.as_mut() else { continue };
        match line {
            "#errors" | "#new-errors" if section != "#document" => section = "#errors",
            "#document-fragment" if section != "#data" && section != "#document" => {
                section = "#document-fragment"
            }
            "#script-on" if section != "#data" && section != "#document" => {
                c.scripting = Some(true)
            }
            "#script-off" if section != "#data" && section != "#document" => {
                c.scripting = Some(false)
            }
            "#document" if section != "#data" => section = "#document",
            _ => match section {
                "#data" => {
                    c.data.push_str(line);
                    c.data.push('\n');
                }
                "#document-fragment" => c.fragment = Some(line.to_string()),
                "#document" => {
                    c.document.push_str(line);
                    c.document.push('\n');
                }
                _ => {}
            },
        }
    }
    if let Some(c) = cur.take() {
        cases.push(finish(c));
    }
    cases
}

fn finish(mut c: Case) -> Case {
    // 数据最后一行的换行不算数据
    if c.data.ends_with('\n') {
        c.data.pop();
    }
    // 用例之间的空行
    while c.document.ends_with("\n\n") {
        c.document.pop();
    }
    c
}
