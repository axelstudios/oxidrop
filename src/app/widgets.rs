//! Custom-painted widgets: progress bars, icons, and the drag-and-drop overlay.

use std::{
    f32::consts::{FRAC_PI_2, PI},
    f64::consts::TAU,
    path::Path,
    time::Duration,
};

use egui::{
    Align2, Atom, Button, Color32, Context, FontId, HoveredFile, Id, LayerId, Order, Painter, Pos2,
    Rect, Response, Sense, Shape, Stroke, TextStyle, Ui, Vec2,
    epaint::{CubicBezierShape, RoundedRect, tessellator::path},
    pos2,
    text::{LayoutJob, TextWrapping},
    vec2,
};

use crate::format::{self, count, grouped};

/// How long one sweep of the indeterminate progress bar takes, in seconds.
const SWEEP_SECONDS: f64 = 1.4;

/// Time between animation frames: about 60 per second, well below high-refresh monitors, so
/// spinners and sweeping bars cost little CPU while files are being optimized.
pub(super) const ANIMATION_INTERVAL: Duration = Duration::from_millis(16);

/// A small rotating arc, like egui's `Spinner` but repainting at [`ANIMATION_INTERVAL`]
/// instead of on every display refresh.
pub(super) fn spinner(ui: &mut Ui, size: f32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let time = ui.input(|i| i.time);
        let start = time * TAU;
        let end = start + 240_f64.to_radians() * time.sin();
        let radius = rect.height() / 2.0 - 1.5;
        let points = (0..=24)
            .map(|i| {
                let angle = start + (end - start) * f64::from(i) / 24.0;
                #[expect(clippy::cast_possible_truncation, reason = "unit-circle coordinates")]
                let (sin, cos) = (angle.sin() as f32, angle.cos() as f32);
                rect.center() + radius * vec2(cos, sin)
            })
            .collect();
        let color = ui.visuals().strong_text_color();
        ui.painter()
            .add(Shape::line(points, Stroke::new(2.0, color)));
        ui.ctx().request_repaint_after(ANIMATION_INTERVAL);
    }
    response
}

/// A rounded progress bar with text inside, and `value` (e.g. `~42%`) at its right end so it
/// stays visible when the text is cut short; cut text ends in "…" and shows whole on hover.
/// With `fraction == None` a highlighted segment sweeps across the bar to show that work is
/// happening without a known end.
pub(super) fn progress_bar(
    ui: &mut Ui,
    fraction: Option<f32>,
    text: &str,
    value: Option<&str>,
) -> Response {
    let height = ui.spacing().interact_size.y;
    let size = vec2(ui.available_width().max(height * 2.0), height);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if !ui.is_rect_visible(rect) {
        return response;
    }

    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    let radius = height / 2.0;
    let fill = visuals.selection.bg_fill;
    painter.rect_filled(rect, radius, visuals.extreme_bg_color);
    if let Some(fraction) = fraction {
        let width = rect.width() * fraction.clamp(0.0, 1.0);
        if width > 0.0 {
            let filled = Rect::from_min_size(rect.min, vec2(width.max(height), height));
            painter.rect_filled(filled, radius, fill);
        }
    } else {
        #[expect(clippy::cast_possible_truncation, reason = "a phase between 0 and 1")]
        let phase = (ui.input(|i| i.time) % SWEEP_SECONDS / SWEEP_SECONDS) as f32;
        let segment = rect.width() * 0.35;
        let left = rect.left() - segment + (rect.width() + segment) * phase;
        let sweep = Rect::from_x_y_ranges(left..=left + segment, rect.y_range());
        painter.rect_filled(sweep, radius, fill);
        ui.ctx().request_repaint_after(ANIMATION_INTERVAL);
    }

    let font = TextStyle::Body.resolve(ui.style());
    let color = visuals.strong_text_color();
    let mut right = rect.right() - radius;
    if let Some(value) = value {
        let galley = painter.layout_no_wrap(value.to_owned(), font.clone(), color);
        right -= galley.size().x;
        let pos = pos2(right, rect.center().y - galley.size().y / 2.0);
        painter.galley(pos, galley, color);
        right -= ui.spacing().item_spacing.x;
    }
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width((right - rect.left() - radius).max(0.0));
    let galley = painter.layout_job(job);
    let elided = galley.elided;
    let pos = pos2(
        rect.left() + radius,
        rect.center().y - galley.size().y / 2.0,
    );
    painter.galley(pos, galley, color);
    if elided {
        response.on_hover_text(text)
    } else {
        response
    }
}

/// A small triangle for the sorted column's header, pointing up for ascending order.
pub(super) fn sort_arrow(ascending: bool) -> Atom<'static> {
    Atom::paint(Vec2::splat(9.0), move |ui, args| {
        let c = args.rect.center();
        let (w, h) = (4.0, 3.0);
        let points = if ascending {
            vec![c + vec2(-w, h), c + vec2(w, h), c + vec2(0.0, -h)]
        } else {
            vec![c + vec2(-w, -h), c + vec2(w, -h), c + vec2(0.0, h)]
        };
        let shape = Shape::convex_polygon(points, args.fallback_text_color, Stroke::NONE);
        ui.painter().add(shape);
    })
}

/// Icons for the toolbar buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolIcon {
    Add,
    Folder,
    Start,
    Stop,
}

/// A button with a painted icon before its text. The icon uses the button's text color, so
/// it dims along with the text when the button is disabled.
pub(super) fn tool_button(icon: ToolIcon, text: &str) -> Button<'_> {
    let atom = Atom::paint(Vec2::splat(12.0), move |ui, args| {
        paint_tool_icon(ui.painter(), args.rect, args.fallback_text_color, icon);
    });
    Button::new((atom, text))
}

fn paint_tool_icon(painter: &Painter, rect: Rect, color: Color32, icon: ToolIcon) {
    let c = rect.center();
    let r = rect.width() / 2.0;
    let stroke = Stroke::new(1.5, color);
    match icon {
        ToolIcon::Add => {
            painter.line_segment([c - vec2(r, 0.0), c + vec2(r, 0.0)], stroke);
            painter.line_segment([c - vec2(0.0, r), c + vec2(0.0, r)], stroke);
        }
        ToolIcon::Folder => {
            let outline = vec![
                c + vec2(-r, 0.75 * r),
                c + vec2(-r, -0.75 * r),
                c + vec2(-0.25 * r, -0.75 * r),
                c + vec2(0.05 * r, -0.45 * r),
                c + vec2(r, -0.45 * r),
                c + vec2(r, 0.75 * r),
            ];
            painter.add(Shape::closed_line(outline, Stroke::new(1.3, color)));
        }
        ToolIcon::Start => {
            let triangle = vec![
                c + vec2(-0.6 * r, -0.85 * r),
                c + vec2(0.85 * r, 0.0),
                c + vec2(-0.6 * r, 0.85 * r),
            ];
            painter.add(Shape::convex_polygon(triangle, color, Stroke::NONE));
        }
        ToolIcon::Stop => {
            painter.rect_filled(Rect::from_center_size(c, Vec2::splat(1.5 * r)), 1.0, color);
        }
    }
}

/// The water drop of the mockup's empty window, `size` points tall: an outline with a curved
/// shine at the lower left, traced from the mockup's 16-unit SVG path, in the text color.
pub(super) fn drop_icon(ui: &mut Ui, size: f32) -> Response {
    const STEPS: u16 = 16;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if ui.is_rect_visible(rect) {
        let scale = size / 16.0;
        let at = |x: f32, y: f32| rect.min + scale * vec2(x, y);
        let curve = |points| {
            CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, Stroke::NONE)
        };
        // Angles run clockwise, since y points down.
        let arc = |center: Pos2, radius: f32, from: f32, to: f32| {
            move |t: f32| {
                let angle = from + (to - from) * t;
                center + scale * radius * vec2(angle.cos(), angle.sin())
            }
        };
        // From the tip down the left side, around the bottom, and up the right side.
        let left = curve([at(8.0, 1.5), at(8.0, 1.5), at(3.5, 7.0), at(3.5, 10.0)]);
        let right = curve([at(12.5, 10.0), at(12.5, 7.0), at(8.0, 1.5), at(8.0, 1.5)]);
        let outline = samples(STEPS, |t| left.sample(t))
            .chain(samples(STEPS, arc(at(8.0, 10.0), 4.5, PI, 0.0)))
            .chain(samples(STEPS, |t| right.sample(t)))
            .collect();
        let shine_arc = arc(at(8.0, 10.5), 2.0, PI, FRAC_PI_2);
        let shine = samples(STEPS / 2, &shine_arc)
            .chain([shine_arc(1.0)])
            .collect();
        let stroke = Stroke::new(0.8 * scale, ui.visuals().text_color());
        ui.painter().add(Shape::closed_line(outline, stroke));
        ui.painter().add(Shape::line(shine, stroke));
    }
    response
}

/// `steps` points along a curve for t from 0 up to, but not including, 1, so the next piece
/// of a path can start at its end without repeating the point.
fn samples(steps: u16, point: impl Fn(f32) -> Pos2) -> impl Iterator<Item = Pos2> {
    (0..steps).map(move |i| point(f32::from(i) / f32::from(steps)))
}

/// A dashed outline inside `rect`, like a CSS `dashed` border: Chromium's dashes for borders
/// under 3 points wide are three times the width, with gaps of twice the width.
pub(super) fn dashed_frame(painter: &Painter, rect: Rect, corner_radius: f32, stroke: Stroke) {
    let inset = stroke.width / 2.0;
    let mut outline = Vec::new();
    path::rounded_rectangle(
        &mut outline,
        RoundedRect::new(rect.shrink(inset), corner_radius - inset),
    );
    if let Some(&start) = outline.first() {
        outline.push(start);
    }
    painter.extend(Shape::dashed_line(
        &outline,
        stroke,
        3.0 * stroke.width,
        2.0 * stroke.width,
    ));
}

/// Small icons for the first table column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusIcon {
    /// Finished with a smaller file.
    Improved,
    /// Converted to PNG, but larger than the original file.
    Converted,
    /// Finished, but the file was already as small as oxipng can make it.
    Unchanged,
    Failed,
    Queued,
    Pending,
}

impl StatusIcon {
    #[must_use]
    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Improved => "Optimized",
            Self::Converted => "Converted to PNG; larger than the original",
            Self::Unchanged => "Already optimized",
            Self::Failed => "Failed",
            Self::Queued => "Queued",
            Self::Pending => "Not started",
        }
    }
}

/// Green that reads well on both themes.
#[must_use]
pub(super) fn success_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x8f, 0xd1, 0x8f)
    } else {
        Color32::from_rgb(0x1d, 0x6e, 0x1d)
    }
}

/// Highlight color for winners and improvements: the mockup's accent blue for each theme.
#[must_use]
pub(super) fn accent_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(0x5a, 0xaa, 0xff)
    } else {
        Color32::from_rgb(0x00, 0x69, 0xb4)
    }
}

/// A thin horizontal bar, `fraction` of `width` long, for comparing sizes.
pub(super) fn size_bar(ui: &mut Ui, width: f32, fraction: f32, highlighted: bool) -> Response {
    let height = 8.0;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let filled = Rect::from_min_size(rect.min, vec2(width * fraction.clamp(0.02, 1.0), height));
    let color = if highlighted {
        accent_color(ui)
    } else {
        ui.visuals().widgets.inactive.bg_fill
    };
    ui.painter().rect_filled(filled, height / 2.0, color);
    response
}

/// Chart of the image data's size after each zopfli pass, drawn like the mockup's sparkline:
/// a line through the sizes, a dot per pass (filled where some block got smaller, hollow
/// where none did), and a dashed marker at the last gain. `totals` are in bytes; `gains`
/// counts the blocks of `blocks` that got smaller on each pass. With too many passes for
/// the width, only the passes with a gain keep a (smaller) dot.
pub(super) fn zopfli_chart(
    ui: &mut Ui,
    totals: &[u64],
    gains: &[usize],
    blocks: usize,
    last_gain: usize,
) -> Response {
    /// Chart height, and its margins: room for dots at the edges, and above the plot for
    /// the "last gain" label.
    const HEIGHT: f32 = 62.0;
    const MARGIN_LEFT: f32 = 6.0;
    const MARGIN_RIGHT: f32 = 10.0;
    const MARGIN_TOP: f32 = 17.0;
    const MARGIN_BOTTOM: f32 = 5.0;
    /// Least room per pass for the dots to stay apart.
    const MIN_DOT_SPACING: f32 = 5.0;

    let size = vec2(ui.available_width().clamp(160.0, 320.0), HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if totals.is_empty() || !ui.is_rect_visible(rect) {
        return response;
    }
    let plot = Rect::from_min_max(
        rect.min + vec2(MARGIN_LEFT, MARGIN_TOP),
        rect.max - vec2(MARGIN_RIGHT, MARGIN_BOTTOM),
    );
    // From the first pass at the top to the last at the bottom; the totals only shrink.
    let low = totals.last().copied().unwrap_or(0);
    let high = totals[0].max(low + 1);
    let steps = (totals.len() - 1).max(1) as f32;
    let x = |i: usize| plot.left() + plot.width() * i as f32 / steps;
    let y = |bytes: u64| {
        let share = high.saturating_sub(bytes) as f32 / (high - low) as f32;
        plot.top() + plot.height() * share.clamp(0.0, 1.0)
    };
    let points: Vec<_> = totals
        .iter()
        .enumerate()
        .map(|(i, &bytes)| pos2(x(i), y(bytes)))
        .collect();

    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    let text = visuals.text_color();
    let separator = visuals.widgets.noninteractive.bg_stroke.color;
    let accent = accent_color(ui);

    if let Some(gain) = points.get(last_gain.saturating_sub(1)) {
        let marker = [
            pos2(gain.x, rect.top() + 2.0),
            pos2(gain.x, rect.bottom() - 2.0),
        ];
        painter.extend(Shape::dashed_line(
            &marker,
            Stroke::new(1.0, separator),
            3.0,
            3.0,
        ));
        let font = FontId::proportional(10.0);
        let label = painter.layout_no_wrap("last gain".to_owned(), font, text);
        // Right of the marker, as in the mockup, unless that would run off the chart.
        let left = if gain.x + 5.0 + label.size().x <= rect.right() {
            gain.x + 5.0
        } else {
            gain.x - 5.0 - label.size().x
        };
        painter.galley(pos2(left, rect.top() + 2.0), label, text);
    }

    painter.line(points.clone(), Stroke::new(1.2, text));
    let crowded = plot.width() / steps < MIN_DOT_SPACING;
    for (&center, &gained) in points.iter().zip(gains) {
        if gained > 0 {
            painter.circle_filled(center, if crowded { 2.0 } else { 3.0 }, accent);
        } else if !crowded {
            painter.circle(center, 2.6, visuals.panel_fill, Stroke::new(1.0, text));
        }
    }

    let hovered = response.hover_pos().and_then(|pos| {
        points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.x - pos.x).abs().total_cmp(&(b.x - pos.x).abs()))
            .map(|(i, _)| i)
    });
    match hovered {
        Some(i) => {
            let gained = gains.get(i).copied().unwrap_or(0);
            let blocks_text = match (i, gained, blocks) {
                (0, _, _) => String::new(),
                (_, 0, 1) => " · no gain".to_owned(),
                (_, 0, _) => " · no block got smaller".to_owned(),
                (_, _, 1) => " · the block got smaller".to_owned(),
                _ => format!(
                    " · {} of {} blocks got smaller",
                    grouped(gained as u64),
                    grouped(blocks as u64)
                ),
            };
            response.on_hover_text(format!(
                "Pass {}: {} ({} bytes){blocks_text}",
                grouped(i as u64 + 1),
                format::bytes(totals[i]),
                grouped(totals[i])
            ))
        }
        None => response,
    }
}

/// Paints a status icon and returns its response (hover it for a description).
pub(super) fn status_icon(ui: &mut Ui, icon: StatusIcon) -> Response {
    let side = ui.spacing().interact_size.y * 0.8;
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
    let painter = ui.painter_at(rect.expand(1.0));
    let c = rect.center();
    let r = side / 2.0;
    let weak = ui.visuals().weak_text_color();
    match icon {
        StatusIcon::Improved => {
            let points = vec![
                c + vec2(-0.65 * r, 0.0),
                c + vec2(-0.2 * r, 0.5 * r),
                c + vec2(0.7 * r, -0.55 * r),
            ];
            painter.line(points, Stroke::new(2.0, success_color(ui)));
        }
        StatusIcon::Converted => {
            let points = vec![
                c + vec2(-0.65 * r, 0.0),
                c + vec2(-0.2 * r, 0.5 * r),
                c + vec2(0.7 * r, -0.55 * r),
            ];
            painter.line(points, Stroke::new(2.0, ui.visuals().warn_fg_color));
        }
        StatusIcon::Unchanged => {
            for dy in [-0.25, 0.25] {
                painter.line_segment(
                    [c + vec2(-0.6 * r, dy * r), c + vec2(0.6 * r, dy * r)],
                    Stroke::new(1.5, weak),
                );
            }
        }
        StatusIcon::Failed => {
            let color = ui.visuals().error_fg_color;
            let triangle = vec![
                c + vec2(0.0, -0.85 * r),
                c + vec2(0.9 * r, 0.75 * r),
                c + vec2(-0.9 * r, 0.75 * r),
            ];
            painter.add(Shape::closed_line(triangle, Stroke::new(1.5, color)));
            painter.line_segment(
                [c + vec2(0.0, -0.3 * r), c + vec2(0.0, 0.2 * r)],
                Stroke::new(1.5, color),
            );
            painter.circle_filled(c + vec2(0.0, 0.48 * r), 1.0, color);
        }
        StatusIcon::Queued => {
            painter.circle_stroke(c, 0.8 * r, Stroke::new(1.3, weak));
            painter.line(
                vec![c + vec2(0.0, -0.5 * r), c, c + vec2(0.35 * r, 0.3 * r)],
                Stroke::new(1.3, weak),
            );
        }
        StatusIcon::Pending => {
            painter.circle_stroke(c, 0.35 * r, Stroke::new(1.3, weak));
        }
    }
    response.on_hover_text(icon.description())
}

/// Dims the window and explains what dropping will do while files are dragged over it.
pub(super) fn drop_overlay(ctx: &Context, hovered: &[HoveredFile]) {
    if hovered.is_empty() {
        return;
    }
    let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::unique("drop-overlay")));
    let screen = ctx.content_rect();
    let dark = ctx.global_style().visuals.dark_mode;
    let (veil, accent, text) = if dark {
        (
            Color32::from_black_alpha(190),
            Color32::from_rgb(0x5a, 0xaa, 0xff),
            Color32::WHITE,
        )
    } else {
        (
            Color32::from_white_alpha(225),
            Color32::from_rgb(0x00, 0x69, 0xb4),
            Color32::BLACK,
        )
    };
    painter.rect_filled(screen, 0.0, veil);

    let frame = screen.shrink(14.0);
    let outline = [
        frame.left_top(),
        frame.right_top(),
        frame.right_bottom(),
        frame.left_bottom(),
        frame.left_top(),
    ];
    painter.extend(Shape::dashed_line(
        &outline,
        Stroke::new(2.0, accent),
        10.0,
        6.0,
    ));

    let (headline, hint) = describe_drop(hovered.iter().filter_map(|f| f.path.as_deref()));
    let center = frame.center();
    painter.text(
        center - vec2(0.0, 14.0),
        Align2::CENTER_CENTER,
        headline,
        FontId::proportional(22.0),
        text,
    );
    painter.text(
        center + vec2(0.0, 16.0),
        Align2::CENTER_CENTER,
        hint,
        FontId::proportional(14.0),
        text,
    );
}

/// Headline and hint for the drop overlay, e.g. "Release to add 3 files and 1 folder".
fn describe_drop<'a>(paths: impl Iterator<Item = &'a Path>) -> (String, String) {
    let (mut files, mut folders) = (0, 0);
    for path in paths {
        if path.is_dir() {
            folders += 1;
        } else {
            files += 1;
        }
    }
    let headline = match (files, folders) {
        (0, 0) => "Release to add files".to_owned(),
        (_, 0) => format!("Release to add {}", count(files, "file", "files")),
        (0, _) => format!("Release to add {}", count(folders, "folder", "folders")),
        _ => format!(
            "Release to add {} and {}",
            count(files, "file", "files"),
            count(folders, "folder", "folders")
        ),
    };
    let hint = if folders > 0 {
        "Folders are searched for PNG, APNG, GIF, and WebP files"
    } else {
        "PNG and APNG files are optimized; GIF and WebP files become new PNGs"
    };
    (headline, hint.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_what_a_drop_will_add() {
        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("a.png");
        std::fs::write(&file, b"x").expect("write");

        let (headline, _) = describe_drop([file.as_path(), file.as_path()].into_iter());
        assert_eq!(headline, "Release to add 2 files");
        let (headline, hint) = describe_drop([file.as_path(), dir.path()].into_iter());
        assert_eq!(headline, "Release to add 1 file and 1 folder");
        assert_eq!(
            hint,
            "Folders are searched for PNG, APNG, GIF, and WebP files"
        );
        assert_eq!(describe_drop(std::iter::empty()).0, "Release to add files");
    }
}
