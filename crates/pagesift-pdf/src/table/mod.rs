//! PDF 表格解析与重构模块。
//! 包含有框线（Lattice）、三线表（Booktabs）与无框线（Stream）多模态提取算法。

pub(crate) mod borderless;
pub(crate) mod grid;
pub(crate) mod lattice;

use crate::interp::Rule;
use crate::layout::Line;
use borderless::{ruled_only, stream};
use grid::{grid, region};
use lattice::{SNAP, lattice, merge};

#[derive(Debug, Clone)]
pub(crate) struct Table {
    pub(crate) x0: f64,
    pub(crate) x1: f64,
    /// 顶部（y 向上，所以是较大的那个）
    pub(crate) top: f64,
    pub(crate) bottom: f64,
    pub(crate) rows: Vec<Vec<String>>,
}

impl Table {
    pub(crate) fn markdown(&self) -> String {
        let cols = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let cell = |s: &str| s.replace('|', "\\|");
        let row = |r: &Vec<String>| {
            let mut s = String::from("|");
            for i in 0..cols {
                s.push(' ');
                s.push_str(&cell(r.get(i).map(String::as_str).unwrap_or("")));
                s.push_str(" |");
            }
            s
        };
        let mut lines = vec![row(&self.rows[0]), format!("|{}", " --- |".repeat(cols))];
        lines.extend(self.rows[1..].iter().map(row));
        lines.join("\n")
    }
}

fn filled(t: &Table) -> usize {
    t.rows.iter().flatten().filter(|c| !c.is_empty()).count()
}

/// 找一页的表格。表格里的片段从 `frags` 里拿掉，返回的表格按从上到下排列。
pub(crate) fn extract(
    rules: &[Rule],
    frags: &mut Vec<Line>,
    page_width: f64,
    page_height: f64,
) -> Vec<Table> {
    if frags.is_empty() {
        return Vec::new();
    }
    let (h, v) = merge(rules);
    let mut used = vec![false; frags.len()];
    let mut tables: Vec<Table> = Vec::new();
    if !v.is_empty() {
        for cells in lattice(&h, &v) {
            if cells.len() >= 2 {
                let mut trial = used.clone();
                if let Some((t, crowded, horiz)) = grid(&cells, frags, &mut trial) {
                    let unruled_body = t.rows.len() <= 2;
                    let should_fallback = (unruled_body
                        && (crowded > 0.3 || cells.iter().any(|c| (c.1 - c.3) > 3.5 * 10.0)))
                        || horiz > 0.25;
                    if should_fallback
                        && let Some((alt, mine)) = region(frags, &used, t.x0, t.x1, t.top, t.bottom)
                        && filled(&alt) > filled(&t)
                    {
                        for k in mine {
                            used[k] = true;
                        }
                        tables.push(alt);
                    } else {
                        used = trial;
                        tables.push(t);
                    }
                    continue;
                }
            }
            let x0 = cells.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
            let x1 = cells.iter().map(|c| c.2).fold(f64::NEG_INFINITY, f64::max);
            let top = cells.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
            let bottom = cells.iter().map(|c| c.3).fold(f64::INFINITY, f64::min);
            if x1 - x0 >= 0.2 * page_width
                && let Some((t, mine)) =
                    region(frags, &used, x0, x1, top, bottom).filter(|(t, _)| t.rows.len() >= 3)
            {
                for k in mine {
                    used[k] = true;
                }
                tables.push(t);
            }
        }
    }
    let free_h: Vec<lattice::HEdge> = h
        .iter()
        .copied()
        .filter(|e| {
            !tables.iter().any(|t| {
                e.y <= t.top + SNAP
                    && e.y >= t.bottom - SNAP
                    && e.x0 >= t.x0 - SNAP
                    && e.x1 <= t.x1 + SNAP
            })
        })
        .collect();
    tables.extend(ruled_only(&free_h, frags, &mut used, page_width));
    tables.extend(stream(frags, &mut used, page_height));
    let mut it = used.iter();
    frags.retain(|_| !it.next().copied().unwrap_or(false));
    tables.sort_by(|a, b| b.top.total_cmp(&a.top));
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(text: &str, x0: f64, y: f64) -> Line {
        let x1 = x0 + 5.0 * text.chars().count() as f64;
        Line {
            text: text.into(),
            x0,
            x1,
            y,
            size: 10.0,
            bold: false,
            raw: false,
            mcid: None,
            artifact: false,
            hint: None,
            group: None,
            keep: false,
        }
    }

    fn hrule(x0: f64, x1: f64, y: f64) -> Rule {
        Rule {
            x0,
            y0: y,
            x1,
            y1: y,
        }
    }

    fn vrule(x: f64, y0: f64, y1: f64) -> Rule {
        Rule {
            x0: x,
            y0,
            x1: x,
            y1,
        }
    }

    #[test]
    fn lattice_grid() {
        let rules = vec![
            hrule(100.0, 300.0, 700.0),
            hrule(100.0, 300.0, 680.0),
            hrule(100.0, 300.0, 660.0),
            vrule(100.0, 660.0, 700.0),
            vrule(200.0, 660.0, 700.0),
            vrule(300.0, 660.0, 700.0),
        ];
        let mut frags = vec![
            frag("Name", 105.0, 686.0),
            frag("Age", 205.0, 686.0),
            frag("Ann", 105.0, 666.0),
            frag("42", 205.0, 666.0),
            frag("body text", 100.0, 500.0),
        ];
        let tables = extract(&rules, &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows, [["Name", "Age"], ["Ann", "42"]]);
        assert_eq!(frags.len(), 1, "table fragments leave the body text");
    }

    #[test]
    fn booktabs_with_wrapped_cells() {
        let rules = vec![
            hrule(100.0, 400.0, 720.0),
            hrule(100.0, 400.0, 700.0),
            hrule(100.0, 400.0, 630.0),
        ];
        let mut frags = vec![
            frag("Step", 100.0, 706.0),
            frag("Date", 250.0, 706.0),
            frag("Begin", 100.0, 686.0),
            frag("May 2", 250.0, 686.0),
            frag("Complete the", 100.0, 666.0),
            frag("June 9", 250.0, 666.0),
            frag("design", 100.0, 654.0),
            frag("Ship", 100.0, 634.0),
            frag("July 1", 250.0, 634.0),
        ];
        let tables = extract(&rules, &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(
            tables[0].rows,
            [
                ["Step", "Date"],
                ["Begin", "May 2"],
                ["Complete the design", "June 9"],
                ["Ship", "July 1"]
            ]
        );
    }

    #[test]
    fn borderless_table() {
        let mut frags = vec![frag(
            "Revenue grew this quarter thanks to strong sales in all regions.",
            50.0,
            700.0,
        )];
        for (i, (k, a, b)) in [
            ("Q1", "$448", "5%"),
            ("Q2", "$427", "7%"),
            ("Q3", "$512", "3%"),
            ("Q4", "$530", "4%"),
        ]
        .iter()
        .enumerate()
        {
            let y = 650.0 - 15.0 * i as f64;
            frags.extend([frag(k, 50.0, y), frag(a, 150.0, y), frag(b, 250.0, y)]);
        }
        let tables = extract(&[], &mut frags, 600.0, 800.0);
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].rows[0], ["Q1", "$448", "5%"]);
        assert_eq!(tables[0].rows.len(), 4);
        assert_eq!(frags.len(), 1);
    }

    #[test]
    fn narrow_columns_of_prose_are_not_a_table() {
        let left = [
            "the committee met in",
            "spring and agreed to",
            "extend the program for",
            "another two years after",
        ];
        let right = [
            "long debate. members said",
            "the results had been",
            "better than expected and",
            "funding would continue",
        ];
        let mut frags = Vec::new();
        for i in 0..4 {
            let y = 700.0 - 12.0 * i as f64;
            frags.push(frag(left[i], 50.0, y));
            frags.push(frag(right[i], 300.0, y));
        }
        assert!(extract(&[], &mut frags, 600.0, 800.0).is_empty());
        assert_eq!(frags.len(), 8);
    }

    #[test]
    fn numbered_list_is_not_a_table() {
        let mut frags = Vec::new();
        for (i, t) in [
            "Open the box",
            "Remove the cover",
            "Plug it in",
            "Press start",
        ]
        .iter()
        .enumerate()
        {
            let y = 700.0 - 14.0 * i as f64;
            frags.push(frag(&format!("{}.", i + 1), 50.0, y));
            frags.push(frag(t, 80.0, y));
        }
        assert!(extract(&[], &mut frags, 600.0, 800.0).is_empty());
    }

    #[test]
    fn footer_with_page_number_is_not_a_table_row() {
        assert!(
            borderless::page_number("42")
                && borderless::page_number("Page 3")
                && borderless::page_number("xii")
                && borderless::page_number("3 / 10")
        );
        assert!(!borderless::page_number("Q3") && !borderless::page_number("$448"));
    }
}
