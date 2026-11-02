//! The Zopfli block of the optimization report: the image data's size after each pass.

use egui::{Label, RichText, Ui};

use super::value;
use crate::{app::widgets, format, progress::ZopfliSummary};

/// The image data's size after each zopfli pass, all blocks added up, as a chart with a
/// summary and advice.
pub(super) fn zopfli_passes(ui: &mut Ui, summary: &ZopfliSummary) {
    ui.spacing_mut().item_spacing.y = 4.0;
    let title = format!(
        "Zopfli passes · image data, {}",
        format::count(summary.blocks, "block", "blocks")
    );
    let explanation = "Each point is the size the image data would have if Zopfli had stopped \
                       after that many passes. Zopfli splits the data into blocks and optimizes \
                       them one after another, so this adds up every block's best size so far; \
                       blocks that stopped earlier count with their final size.";
    ui.label(title).on_hover_text(explanation);
    widgets::zopfli_chart(
        ui,
        &summary.totals,
        &summary.gains,
        summary.blocks,
        summary.last_gain,
    )
    .on_hover_text(explanation);
    let (first, last) = (
        summary.totals.first().copied().unwrap_or(0),
        summary.totals.last().copied().unwrap_or(0),
    );
    ui.add(
        Label::new(value(format!(
            "{} after pass 1, {} after pass {} ({}) · last gain at pass {}",
            format::bytes(first),
            format::bytes(last),
            format::grouped(summary.totals.len() as u64),
            format::savings(first, last),
            format::grouped(summary.last_gain as u64)
        )))
        .wrap(),
    );
    if let Some(hint) = summary.hint() {
        ui.add(Label::new(RichText::new(hint).weak().size(12.0)).wrap());
    }
}
