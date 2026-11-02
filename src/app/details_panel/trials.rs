//! The trials block of the optimization report: the reduction candidates and the filter trials
//! as bar charts.

use egui::{Align, Label, Layout, RichText, Ui, vec2};

use super::{ROW_GAP, value};
use crate::{
    app::widgets,
    format,
    progress::{Candidate, TrialSize},
};

/// Width of the size column next to the trial bars, and the shortest a bar gets.
const TRIAL_SIZE_WIDTH: f32 = 62.0;
const MIN_BAR_WIDTH: f32 = 40.0;
/// Longest list of candidates shown as bars.
const MAX_BARS: usize = 6;

/// Reduction candidates as bars, smallest (chosen) first, then the filter trials compared
/// with the winner.
pub(super) fn trials(ui: &mut Ui, reductions: &[Candidate], filters: &[Candidate]) {
    ui.spacing_mut().item_spacing.y = ROW_GAP;
    // A chart needs two rows to compare anything.
    let reduction_bars: Vec<Bar> = if reductions.len() > 1 {
        reductions
            .iter()
            .map(|c| Bar {
                label: c.label.clone(),
                bytes: c.size.bytes(),
                value: size_text(c.size),
            })
            .collect()
    } else {
        Vec::new()
    };
    let filter_bars = match filters.split_first() {
        Some((winner, rest)) if !rest.is_empty() => filter_bars(winner, rest),
        _ => Vec::new(),
    };
    // Both charts share their columns, so their bars line up.
    let columns = chart_columns(ui, [reduction_bars.as_slice(), &filter_bars]);
    if !reduction_bars.is_empty() {
        ui.label("Reductions tried · quick trial sizes")
            .on_hover_text("oxipng compresses each candidate quickly to pick the best one.");
        bar_chart(ui, &reduction_bars, columns);
        ui.add_space(4.0);
    }
    if !filter_bars.is_empty() {
        ui.label("Filters tried · compared with the winner")
            .on_hover_text(
                "Sizes from oxipng's filter trials: the smallest, then how much larger each \
                 other filter came out. \"Stopped\" trials were cut short once they grew past \
                 the best; their bars end where they stopped.",
            );
        bar_chart(ui, &filter_bars, columns);
    } else if let Some(only) = filters.first() {
        ui.label("Filter tried");
        ui.label(value(format!("{} {}", only.label, size_text(only.size))));
    }
}

/// One row of a trial chart.
#[derive(Debug, PartialEq)]
struct Bar {
    label: String,
    /// The bar's length; for a trial cut short, the size where it stopped.
    bytes: usize,
    /// Shown after the bar.
    value: String,
}

/// The filter trials as bars: the winner with its size, the others with how much larger
/// they came out.
fn filter_bars(winner: &Candidate, rest: &[Candidate]) -> Vec<Bar> {
    let best = winner.size.bytes().max(1) as f64;
    let first = Bar {
        label: winner.label.clone(),
        bytes: winner.size.bytes(),
        value: size_text(winner.size),
    };
    std::iter::once(first)
        .chain(rest.iter().map(|candidate| Bar {
            label: candidate.label.clone(),
            bytes: candidate.size.bytes(),
            value: match candidate.size {
                TrialSize::Exact(bytes) => format!(
                    "+{}%",
                    format::decimal((bytes as f64 - best) / best * 100.0, 1)
                ),
                TrialSize::Over(_) => "stopped".to_owned(),
            },
        }))
        .collect()
}

/// Widths of a trial chart's name, bar, and value columns.
#[derive(Debug, Clone, Copy)]
struct ChartColumns {
    name: f32,
    bar: f32,
    value: f32,
}

/// Gap between a trial chart's columns.
const CHART_GAP: f32 = 8.0;

/// Columns that fit the rows shown of every chart in `charts`: names as wide as the longest,
/// values as wide as the widest (at least a size's width), as long as the bars keep some
/// length.
fn chart_columns<'a>(ui: &Ui, charts: impl IntoIterator<Item = &'a [Bar]>) -> ChartColumns {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let width = |text: &str| {
        ui.painter()
            .layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE)
            .size()
            .x
    };
    let (mut longest, mut value) = (0.0, TRIAL_SIZE_WIDTH);
    for bar in charts
        .into_iter()
        .flat_map(|bars| &bars[..bars.len().min(MAX_BARS)])
    {
        longest = f32::max(longest, width(&bar.label));
        value = f32::max(value, width(&bar.value));
    }
    let room = ui.available_width() - value - 2.0 * CHART_GAP;
    let name = f32::min(longest, room - MIN_BAR_WIDTH).max(0.0);
    ChartColumns {
        name,
        bar: (room - name).max(MIN_BAR_WIDTH),
        value,
    }
}

/// Trial results as bars, the first (the winner) highlighted, each as long as its size
/// relative to the largest. Rows past `MAX_BARS` are listed on one line below.
fn bar_chart(ui: &mut Ui, bars: &[Bar], columns: ChartColumns) {
    let shown = &bars[..bars.len().min(MAX_BARS)];
    let max = shown.iter().map(|bar| bar.bytes).max().unwrap_or(1).max(1);
    let row_height = ui.text_style_height(&egui::TextStyle::Body);
    let ChartColumns {
        name: name_width,
        bar: bar_width,
        value: value_width,
    } = columns;
    for (i, bar) in shown.iter().enumerate() {
        let winner = i == 0;
        let style = |text: String| {
            if winner {
                value(text)
            } else {
                RichText::new(text)
            }
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = CHART_GAP;
            ui.allocate_ui_with_layout(
                vec2(name_width, row_height),
                Layout::left_to_right(Align::Center),
                |ui| {
                    ui.set_width(name_width);
                    ui.add(Label::new(style(bar.label.clone())).truncate());
                },
            );
            widgets::size_bar(ui, bar_width, bar.bytes as f32 / max as f32, winner);
            ui.allocate_ui_with_layout(
                vec2(value_width, row_height),
                Layout::right_to_left(Align::Center),
                |ui| {
                    ui.set_width(value_width);
                    ui.label(style(bar.value.clone()));
                },
            );
        });
    }
    let rest: Vec<String> = bars[shown.len()..]
        .iter()
        .map(|bar| format!("{} {}", bar.label, bar.value))
        .collect();
    if !rest.is_empty() {
        ui.add(Label::new(RichText::new(rest.join(" · ")).weak()).wrap());
    }
}

fn size_text(size: TrialSize) -> String {
    match size {
        TrialSize::Exact(bytes) => format::bytes(bytes as u64),
        TrialSize::Over(bytes) => format!("> {}", format::bytes(bytes as u64)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charts_filters_against_the_winner() {
        let candidate = |label: &str, size| Candidate {
            label: label.to_owned(),
            size,
        };
        let bars = filter_bars(
            &candidate("Bigrams", TrialSize::Exact(30_515)),
            &[
                candidate("BigEnt", TrialSize::Exact(30_820)),
                candidate("None", TrialSize::Exact(34_726)),
                candidate("Sub", TrialSize::Over(30_515)),
            ],
        );
        let values: Vec<&str> = bars.iter().map(|bar| bar.value.as_str()).collect();
        assert_eq!(values, ["29.8 KiB", "+1.0%", "+13.8%", "stopped"]);
        assert_eq!(
            bars[3].bytes, 30_515,
            "a cut-short bar ends where it stopped"
        );
    }
}
