//! The virtualized grid used for D1 table pages and SQL results.

use gpui::prelude::*;
use gpui::{App, SharedString, Window, div, px};
use gpui_component::ActiveTheme as _;
use gpui_component::table::{Column, TableDelegate, TableState};
use serde_json::Value;

use crate::cloudflare::d1::display_value;

#[derive(Default)]
pub struct ResultGrid {
    columns: Vec<SharedString>,
    widths: Vec<f32>,
    rows: Vec<Vec<Cell>>,
    pub loading: bool,
}

#[derive(Clone)]
struct Cell {
    text: SharedString,
    null: bool,
}

impl ResultGrid {
    pub fn set(&mut self, columns: Vec<String>, rows: &[Vec<Value>]) {
        self.rows = rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|v| Cell {
                        // Long values would make every row tall; the grid shows one line.
                        text: one_line(&display_value(v)).into(),
                        null: v.is_null(),
                    })
                    .collect()
            })
            .collect();
        self.widths = columns
            .iter()
            .enumerate()
            .map(|(ix, name)| {
                let longest = self
                    .rows
                    .iter()
                    .take(100)
                    .filter_map(|r| r.get(ix))
                    .map(|c| c.text.chars().count())
                    .max()
                    .unwrap_or(0)
                    .max(name.chars().count());
                (longest as f32 * 8.6 + 28.).clamp(70., 420.)
            })
            .collect();
        self.columns = columns.into_iter().map(SharedString::from).collect();
        self.loading = false;
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

fn one_line(s: &str) -> String {
    const MAX: usize = 500;
    let flat: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .take(MAX + 1)
        .collect();
    if flat.chars().count() > MAX {
        flat.chars().take(MAX).collect::<String>() + "…"
    } else {
        flat
    }
}

impl TableDelegate for ResultGrid {
    fn columns_count(&self, _: &App) -> usize {
        self.columns.len()
    }

    fn rows_count(&self, _: &App) -> usize {
        self.rows.len()
    }

    fn column(&self, ix: usize, _: &App) -> Column {
        // Keys must be unique, and SQL results can repeat a column name.
        Column::new(format!("c{ix}"), self.columns[ix].clone())
            .width(px(self.widths.get(ix).copied().unwrap_or(120.)))
            .min_width(px(40.))
            .movable(false)
    }

    fn render_td(
        &mut self,
        row: usize,
        col: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let cell = self.rows.get(row).and_then(|r| r.get(col)).cloned();
        div()
            .truncate()
            .text_sm()
            .font_family(cx.theme().mono_font_family.clone())
            .when_some(cell, |this, cell| {
                this.when(cell.null, |this| {
                    this.text_color(cx.theme().muted_foreground)
                })
                .child(cell.text)
            })
    }

    fn loading(&self, _: &App) -> bool {
        self.loading
    }

    fn cell_text(&self, row: usize, col: usize, _: &App) -> String {
        self.rows
            .get(row)
            .and_then(|r| r.get(col))
            .map(|c| c.text.to_string())
            .unwrap_or_default()
    }
}

/// Swaps in a new result set and resets the grid's columns and scroll.
pub fn show_results(
    table: &gpui::Entity<TableState<ResultGrid>>,
    columns: Vec<String>,
    rows: &[Vec<Value>],
    cx: &mut App,
) {
    table.update(cx, |t, cx| {
        t.delegate_mut().set(columns, rows);
        t.refresh(cx);
        if t.delegate().row_count() > 0 {
            t.scroll_to_row(0, cx);
        }
        cx.notify();
    });
}

pub fn set_loading(table: &gpui::Entity<TableState<ResultGrid>>, loading: bool, cx: &mut App) {
    table.update(cx, |t, cx| {
        t.delegate_mut().loading = loading;
        cx.notify();
    });
}

/// `1234567` → `1,234,567`.
pub fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn fmt_ms(ms: f64) -> String {
    if ms < 1.0 {
        format!("{ms:.2} ms")
    } else if ms < 1000.0 {
        format!("{ms:.1} ms")
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts_and_durations() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1000), "1,000");
        assert_eq!(fmt_count(1234567), "1,234,567");
        assert_eq!(fmt_ms(0.208), "0.21 ms");
        assert_eq!(fmt_ms(12.34), "12.3 ms");
        assert_eq!(fmt_ms(2500.0), "2.50 s");
    }

    #[test]
    fn cells_are_flattened_to_one_line() {
        assert_eq!(one_line("a\nb\r\nc"), "a b  c");
        let long = "x".repeat(600);
        assert_eq!(one_line(&long).chars().count(), 501);
    }

    #[test]
    fn counts_group_every_three_digits() {
        assert_eq!(fmt_count(100_000), "100,000");
        assert_eq!(fmt_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn durations_switch_units_at_the_boundaries() {
        assert_eq!(fmt_ms(0.0), "0.00 ms");
        assert_eq!(fmt_ms(1.0), "1.0 ms");
        assert_eq!(fmt_ms(999.0), "999.0 ms");
        assert_eq!(fmt_ms(1000.0), "1.00 s");
        assert_eq!(fmt_ms(61_000.0), "61.00 s");
    }

    #[test]
    #[ignore = "bug: 999.96 ms rounds up and is shown as \"1000.0 ms\" instead of \"1.00 s\""]
    fn durations_just_under_a_second_do_not_round_to_1000_ms() {
        assert_eq!(fmt_ms(999.96), "1.00 s");
    }

    #[test]
    fn one_line_keeps_exactly_the_limit_and_counts_characters() {
        let exact = "é".repeat(500);
        assert_eq!(one_line(&exact), exact);
        let over = "é".repeat(501);
        assert_eq!(one_line(&over), "é".repeat(500) + "…");
        assert_eq!(one_line(""), "");
    }

    #[test]
    fn grid_cells_are_displayed_flattened_and_null_marked() {
        let mut grid = ResultGrid {
            loading: true,
            ..Default::default()
        };
        let rows = vec![
            vec![serde_json::json!(1), serde_json::Value::Null],
            vec![serde_json::json!([0, 1]), serde_json::json!("two\nlines")],
        ];
        grid.set(vec!["id".into(), "note".into()], &rows);
        assert!(!grid.loading);
        assert_eq!(grid.row_count(), 2);
        let texts: Vec<Vec<&str>> = grid
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.text.as_ref()).collect())
            .collect();
        assert_eq!(texts, [["1", "NULL"], ["<blob 2 bytes>", "two lines"]]);
        let nulls: Vec<Vec<bool>> = grid
            .rows
            .iter()
            .map(|r| r.iter().map(|c| c.null).collect())
            .collect();
        assert_eq!(nulls, [[false, true], [false, false]]);
    }

    #[test]
    fn column_widths_fit_content_within_limits() {
        let mut grid = ResultGrid::default();
        let long = "x".repeat(200);
        grid.set(
            vec!["a".into(), "a_rather_long_column_name".into(), "c".into()],
            &[vec![
                serde_json::json!("y"),
                serde_json::json!("z"),
                serde_json::json!(long),
            ]],
        );
        assert_eq!(grid.widths[0], 70., "narrow columns get a minimum");
        assert_eq!(grid.widths[1], 25. * 8.6 + 28., "headers count too");
        assert_eq!(grid.widths[2], 420., "wide columns are capped");
        assert_eq!(grid.columns.len(), 3);
    }

    #[test]
    fn short_rows_do_not_break_widths() {
        let mut grid = ResultGrid::default();
        grid.set(vec!["a".into(), "b".into()], &[vec![serde_json::json!(1)]]);
        assert_eq!(grid.widths.len(), 2);
        assert_eq!(grid.widths[1], 70.);
    }
}
