use super::*;

fn line(text: &str, x0: f64, y: f64, size: f64) -> Line {
    Line {
        text: text.into(),
        x0,
        x1: x0 + 0.5 * size * text.chars().count() as f64,
        y,
        size,
        bold: false,
        raw: false,
        mcid: None,
        artifact: false,
        hint: None,
        group: None,
        keep: false,
        turned: false,
    }
}

fn polyline(x0: f64, y0: f64, x1: f64, y1: f64, segs: u16) -> Shape {
    Shape {
        x0,
        y0,
        x1,
        y1,
        slanted: segs,
        curves: 0,
        rect: false,
        fill: false,
        visible: true,
    }
}

#[test]
fn line_chart_claims_axis_labels_not_body() {
    // 折线 + 纵轴刻度值 + 横轴年份；上方题注、下方正文不归图
    let shapes = vec![polyline(100.0, 400.0, 400.0, 550.0, 60)];
    let mut lines = vec![
        line(
            "Figure 3. Prices rose sharply in the second half of the year",
            100.0,
            580.0,
            10.0,
        ),
        line("Percent", 100.0, 560.0, 8.0),
    ];
    for (k, v) in ["8", "6", "4", "2", "0"].iter().enumerate() {
        lines.push(line(v, 88.0, 545.0 - 35.0 * k as f64, 8.0));
    }
    lines.push(line("2019 2020 2021 2022 2023", 120.0, 388.0, 8.0));
    lines.push(line(
        "The committee noted that inflation had eased considerably over the past year and that",
        72.0,
        340.0,
        10.0,
    ));
    let figs = detect(&shapes, &[], &lines, 612.0, 792.0);
    assert_eq!(figs.len(), 1);
    let mut got: Vec<usize> = figs[0].lines.clone();
    got.sort_unstable();
    assert_eq!(got, (1..8).collect::<Vec<_>>());
}

#[test]
fn rounded_text_box_is_not_a_figure() {
    // 四个圆角的框里是正文：文本框，不是图
    let shapes = vec![Shape {
        x0: 50.0,
        y0: 300.0,
        x1: 550.0,
        y1: 500.0,
        slanted: 0,
        curves: 4,
        rect: false,
        fill: false,
        visible: true,
    }];
    let lines: Vec<Line> = (0..8)
        .map(|k| {
            line(
                "Banks have continued to tighten lending standards for most loan types this year",
                60.0,
                480.0 - 20.0 * k as f64,
                10.0,
            )
        })
        .collect();
    assert!(detect(&shapes, &[], &lines, 612.0, 792.0).is_empty());
}

#[test]
fn shaded_table_cells_are_not_bars() {
    // 带文字的格子底色不是柱
    let mut shapes = Vec::new();
    let mut lines = Vec::new();
    for r in 0..6 {
        let y = 600.0 - 20.0 * r as f64;
        shapes.push(Shape {
            x0: 100.0,
            y0: y - 5.0,
            x1: 400.0,
            y1: y + 12.0,
            slanted: 0,
            curves: 0,
            rect: true,
            fill: true,
            visible: true,
        });
        lines.push(line("Revenue", 105.0, y, 9.0));
        lines.push(line("4,212", 300.0, y, 9.0));
    }
    assert!(detect(&shapes, &[], &lines, 612.0, 792.0).is_empty());
}

#[test]
fn labels() {
    assert!(
        caption_like("Figure 2.3. Key Channels")
            && caption_like("図表34：ＣＰＩ")
            && caption_like("Sources: IMF staff.")
    );
    assert!(!caption_like("Tables are shown below"));
    assert!(
        label_like("−0.5")
            && label_like("（季節調整済年率換算、兆円）")
            && label_like("US HY (right scale)")
    );
    assert!(!label_like("みられる。こうしたもとで、先行きの失業率は、"));
    assert!(prose_like(
        "The stock price reaction has been centered in the technology sector, especially"
    ));
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Shape {
    Shape {
        x0,
        y0,
        x1,
        y1,
        slanted: 0,
        curves: 0,
        rect: true,
        fill: true,
        visible: true,
    }
}

#[test]
fn bar_chart_with_baseline_and_labels() {
    // 共用底边、高低不一、隔得较开的柱：是柱形图，下方的年份、左边的刻度值归图
    let shapes: Vec<Shape> = [60.0, 25.0, 90.0, 45.0, 70.0, 30.0, 80.0]
        .iter()
        .enumerate()
        .map(|(k, &h)| {
            rect(
                120.0 + 50.0 * k as f64,
                400.0,
                140.0 + 50.0 * k as f64,
                400.0 + h,
            )
        })
        .collect();
    let mut lines = vec![line(
        "2017 2018 2019 2020 2021 2022 2023",
        120.0,
        388.0,
        8.0,
    )];
    for (k, v) in ["100", "50", "0"].iter().enumerate() {
        lines.push(line(v, 100.0, 485.0 - 42.0 * k as f64, 8.0));
    }
    let figs = detect(&shapes, &[], &lines, 612.0, 792.0);
    assert_eq!(figs.len(), 1);
    assert_eq!(figs[0].lines.len(), 4, "{figs:?}");
}

#[test]
fn table_stripes_are_not_bars() {
    // 隔行底色：同样大小的薄条排成一列，不是柱
    let mut shapes = Vec::new();
    for r in 0..10 {
        let y = 300.0 + 25.0 * r as f64;
        shapes.push(rect(64.0, y, 118.0, y + 4.0));
        shapes.push(rect(118.0, y, 300.0, y + 4.0));
        shapes.push(rect(300.0, y, 554.0, y + 4.0));
    }
    assert!(detect(&shapes, &[], &[], 612.0, 792.0).is_empty());
}

#[test]
fn white_fills_are_ignored() {
    let shapes: Vec<Shape> = [60.0, 25.0, 90.0, 45.0, 70.0, 30.0, 55.0]
        .iter()
        .enumerate()
        .map(|(k, &h)| Shape {
            visible: false,
            ..rect(
                120.0 + 30.0 * k as f64,
                400.0,
                140.0 + 30.0 * k as f64,
                400.0 + h,
            )
        })
        .collect();
    assert!(detect(&shapes, &[], &[], 612.0, 792.0).is_empty());
}

#[test]
fn stacked_bar_panels_stay_apart() {
    // 上下排的两张柱形图：柱同宽、左右错开一点，中间隔着下面那张图的标题和上面那张图的横轴标签
    let mut shapes = Vec::new();
    for (k, &h) in [30.0, 50.0, 20.0, 40.0, 60.0, 25.0].iter().enumerate() {
        let x = 120.0 + 40.0 * k as f64;
        shapes.push(rect(x, 500.0, x + 15.0, 500.0 + h));
        shapes.push(rect(x + 20.0, 380.0, x + 35.0, 380.0 + h));
    }
    // 每根柱下面一个年份
    let mut lines = vec![line("Figure 2. Lending", 120.0, 470.0, 8.0)];
    for k in 0..6 {
        let x = 120.0 + 40.0 * k as f64;
        let year = format!("{}", 2019 + k);
        lines.push(line(&year, x, 488.0, 8.0));
        lines.push(line(&year, x + 20.0, 368.0, 8.0));
    }
    let figs = detect(&shapes, &[], &lines, 612.0, 792.0);
    assert_eq!(figs.len(), 2, "{figs:?}");
}
