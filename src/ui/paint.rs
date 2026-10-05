//! Painting: annotations (mirroring the export renderer via the shared
//! helpers in `annotate`), drag previews, and the selection/region chrome.

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Pos2, Rect, Shape as EguiShape, Stroke,
    StrokeKind, Vec2,
    epaint::{EllipseShape, TextShape},
};

use crate::annotate::{
    Annotation, Shape, Style, Tool, arrow_geometry, highlight_color, marker_radius,
};
use crate::editor::Editor;
use crate::editor::geometry::{
    ITEM_HANDLE_SIZE, KNOB_RADIUS, REGION_GRIP_RADIUS, REGION_HANDLE_SIZE, region_move_grip,
    rotate_around, selection_handles, subtract_rect,
};
use crate::editor::hit::{Measure, item_handles, outer_bbox};
use crate::editor::state::{EditorState, ItemDragKind};
use crate::ui::ACCENT;
use crate::view::View;

/// Draw one annotation onto the canvas.
/// The four corners of `rect` rotated around its center by `rot`, projected
/// to screen space — the one way every surface (shape, chrome, hover
/// outline) maps a rotated box, so they can't drift apart.
fn rotated_screen_corners(view: &View, canvas: Rect, rect: Rect, rot: f32) -> [Pos2; 4] {
    let c = rect.center();
    [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()]
        .map(|q| view.to_screen(canvas, rotate_around(q, c, rot)))
}

pub fn paint_annotation(painter: &egui::Painter, view: &View, canvas: Rect, ann: &Annotation) {
    let to = |p| view.to_screen(canvas, p);
    let zoom = view.zoom;
    let rot = ann.rotation;
    // Keep hairlines visible when zoomed way out; export uses true width.
    let stroke = Stroke::new((ann.style.width * zoom).max(1.0), ann.style.color);
    match &ann.shape {
        Shape::Pen { points } => {
            if points.len() == 1 {
                painter.circle_filled(
                    to(points[0]),
                    (ann.style.width * 0.5 * zoom).max(0.75),
                    ann.style.color,
                );
            } else {
                let pts: Vec<Pos2> = points.iter().map(|p| to(*p)).collect();
                painter.add(EguiShape::line(pts, stroke));
            }
        }
        Shape::Line { a, b } => {
            painter.line_segment([to(*a), to(*b)], stroke);
        }
        Shape::Arrow { a, b } => {
            paint_arrow(painter, view, canvas, *a, *b, &ann.style);
        }
        Shape::Rect { rect } => {
            if rot == 0.0 {
                let r = view.rect_to_screen(canvas, *rect);
                painter.rect_stroke(r, CornerRadius::ZERO, stroke, StrokeKind::Middle);
            } else {
                let pts = rotated_screen_corners(view, canvas, *rect, rot).to_vec();
                painter.add(EguiShape::closed_line(pts, stroke));
            }
        }
        Shape::Ellipse { rect } => {
            let r = view.rect_to_screen(canvas, *rect);
            painter.add(EguiShape::Ellipse(EllipseShape {
                center: r.center(),
                radius: r.size() * 0.5,
                fill: Color32::TRANSPARENT,
                stroke,
                angle: rot,
            }));
        }
        Shape::Highlight { rect } => {
            let fill = highlight_color(ann.style.color);
            if rot == 0.0 {
                let r = view.rect_to_screen(canvas, *rect);
                painter.rect_filled(r, CornerRadius::ZERO, fill);
            } else {
                let pts = rotated_screen_corners(view, canvas, *rect, rot).to_vec();
                painter.add(EguiShape::convex_polygon(pts, fill, Stroke::NONE));
            }
        }
        // Baked into the texture; nothing to draw here.
        Shape::Pixelate { .. } => {}
        Shape::Text { pos, text } => {
            let font = FontId::proportional(ann.style.font_size * zoom);
            let galley = painter.layout_no_wrap(text.clone(), font, ann.style.color);
            if rot == 0.0 {
                painter.galley(to(*pos), galley, ann.style.color);
            } else {
                let c = *pos + (galley.size() / zoom) * 0.5;
                let mut shape =
                    TextShape::new(to(rotate_around(*pos, c, rot)), galley, ann.style.color);
                shape.angle = rot;
                painter.add(EguiShape::Text(shape));
            }
        }
        Shape::Marker { pos, number, target } => {
            // Arrow first; the circle covers the shaft's root.
            if let Some(target) = target {
                paint_arrow(painter, view, canvas, *pos, *target, &ann.style);
            }
            let center = to(*pos);
            painter.circle_filled(center, marker_radius(&ann.style) * zoom, ann.style.color);
            painter.text(
                center,
                Align2::CENTER_CENTER,
                number.to_string(),
                FontId::proportional(ann.style.font_size * zoom),
                Color32::WHITE,
            );
        }
    }
}

/// Shaft + filled head from `a` to `b` (shared by arrows and markers).
fn paint_arrow(
    painter: &egui::Painter,
    view: &View,
    canvas: Rect,
    a: Pos2,
    b: Pos2,
    style: &Style,
) {
    let stroke = Stroke::new((style.width * view.zoom).max(1.0), style.color);
    let geo = arrow_geometry(a, b, style.width);
    painter.line_segment(
        [view.to_screen(canvas, a), view.to_screen(canvas, geo.shaft_end)],
        stroke,
    );
    let head: Vec<Pos2> = geo.head.iter().map(|p| view.to_screen(canvas, *p)).collect();
    painter.add(EguiShape::convex_polygon(head, style.color, Stroke::NONE));
}

/// Live preview of the annotation being dragged out.
pub fn paint_drag_preview(painter: &egui::Painter, editor: &Editor, canvas: Rect) {
    let EditorState::DrawingShape { start, current, points } = &editor.state else { return };
    let (start, current) = (*start, *current);
    let view = &editor.view;
    let style = editor.style;
    let rect = Rect::from_two_pos(start, current);
    match editor.tool {
        Tool::Pen => paint_annotation(
            painter,
            view,
            canvas,
            &Annotation::new(Shape::Pen { points: points.clone() }, style),
        ),
        Tool::Line | Tool::Arrow => {
            let shape = if editor.tool == Tool::Line {
                Shape::Line { a: start, b: current }
            } else {
                Shape::Arrow { a: start, b: current }
            };
            paint_annotation(painter, view, canvas, &Annotation::new(shape, style));
        }
        Tool::Rect | Tool::Ellipse | Tool::Highlight => {
            let shape = match editor.tool {
                Tool::Rect => Shape::Rect { rect },
                Tool::Ellipse => Shape::Ellipse { rect },
                _ => Shape::Highlight { rect },
            };
            paint_annotation(painter, view, canvas, &Annotation::new(shape, style));
        }
        Tool::Pixelate => {
            let r = view.rect_to_screen(canvas, rect);
            painter.rect_filled(r, CornerRadius::ZERO, Color32::from_black_alpha(90));
            painter.rect_stroke(
                r,
                CornerRadius::ZERO,
                Stroke::new(1.0, Color32::WHITE),
                StrokeKind::Middle,
            );
        }
        Tool::Marker => {
            // Marker pinned at the start, arrow following the pointer once
            // the drag clears the circle — the same rule drop_marker applies.
            let target = crate::annotate::marker_target(start, Some(current), &style);
            paint_annotation(
                painter,
                view,
                canvas,
                &Annotation::new(
                    Shape::Marker { pos: start, number: editor.doc.marker_next, target },
                    style,
                ),
            );
        }
        Tool::Select | Tool::Text => {}
    }
}

/// Screen-px the dotted selection box sits outside the shape's ink, so it
/// reads as a frame around the shape rather than a line drawn over it.
const SELECTION_GAP: f32 = 3.0;

/// A dotted selection box a hair outside `bbox` (image coords), rotated by
/// `rotation`.
fn dashed_box(painter: &egui::Painter, view: &View, canvas: Rect, bbox: Rect, rotation: f32) {
    let bbox = bbox.expand(SELECTION_GAP / view.zoom);
    let corners = rotated_screen_corners(view, canvas, bbox, rotation);
    for i in 0..4 {
        painter.extend(EguiShape::dashed_line(
            &[corners[i], corners[(i + 1) % 4]],
            Stroke::new(1.0, ACCENT),
            5.0,
            4.0,
        ));
    }
}

/// A dotted box around every selected item — including the text being typed —
/// plus handles and knobs when a single item is selected.
pub fn paint_selection_chrome(
    painter: &egui::Painter,
    editor: &Editor,
    canvas: Rect,
    measure: Measure,
) {
    // The text being edited is hidden from the canvas and its stored shape is
    // stale, so box the live buffer instead — whatever tool opened the editor,
    // and unrotated, to match the inline editor.
    if let EditorState::TextEditing(edit) = &editor.state {
        let bbox = Rect::from_min_size(edit.pos, measure(&edit.buffer, edit.style.font_size));
        dashed_box(painter, &editor.view, canvas, bbox, 0.0);
    }

    if editor.tool != Tool::Select {
        return;
    }
    let editing = editor.editing_target();
    let single = editor.single_selected();
    for id in &editor.selected {
        // The editing target's box is the live one drawn above, and it shows
        // no handles while typing.
        if editing == Some(*id) {
            continue;
        }
        let Some(ann) = editor.doc.get(*id) else { continue };
        dashed_box(painter, &editor.view, canvas, outer_bbox(ann, measure), ann.rotation);
        if single != Some(*id) {
            continue;
        }
        for (pos, kind) in item_handles(ann, &editor.view, canvas, measure) {
            match kind {
                ItemDragKind::Rotate => {
                    painter.circle_filled(pos, KNOB_RADIUS, Color32::WHITE);
                    painter.circle_stroke(pos, KNOB_RADIUS, Stroke::new(1.5, ACCENT));
                    paint_rotate_icon(painter, pos, 6.0, Color32::from_gray(45));
                }
                ItemDragKind::Move => {
                    painter.circle_filled(pos, KNOB_RADIUS, ACCENT);
                    painter.circle_stroke(pos, KNOB_RADIUS, Stroke::new(1.5, Color32::WHITE));
                    paint_move_icon(painter, pos, 7.0, Color32::WHITE);
                }
                _ => {
                    let r = Rect::from_center_size(pos, Vec2::splat(ITEM_HANDLE_SIZE));
                    painter.rect_filled(r, 1.0, Color32::WHITE);
                    painter.rect_stroke(r, 1.0, Stroke::new(1.0, ACCENT), StrokeKind::Middle);
                }
            }
        }
    }
}

/// Soft outline around the hoverable item so it reads as clickable before
/// committing to a selection.
pub fn paint_hover_outline(
    painter: &egui::Painter,
    editor: &Editor,
    canvas: Rect,
    ann: &Annotation,
    measure: Measure,
) {
    let bbox = outer_bbox(ann, measure).expand(6.0 / editor.view.zoom);
    let pts = rotated_screen_corners(&editor.view, canvas, bbox, ann.rotation).to_vec();
    painter.add(EguiShape::closed_line(pts, Stroke::new(2.0, ACCENT.gamma_multiply(0.6))));
}

/// The multi-select band while shift-dragging.
pub fn paint_rubber_band(painter: &egui::Painter, editor: &Editor, canvas: Rect) {
    let EditorState::RubberBand { anchor, current, .. } = &editor.state else { return };
    let r = editor.view.rect_to_screen(canvas, Rect::from_two_pos(*anchor, *current));
    painter.rect_filled(r, CornerRadius::ZERO, ACCENT.gamma_multiply(0.15));
    painter.rect_stroke(r, CornerRadius::ZERO, Stroke::new(1.0, ACCENT), StrokeKind::Middle);
}

/// Region overlay: dim everything outside the box, border, handles, and the
/// dimensions label — or a whole-canvas dim when no region exists yet.
pub fn paint_region(painter: &egui::Painter, editor: &Editor, canvas: Rect) {
    if let Some(region) = editor.doc.region {
        let ss = editor.view.rect_to_screen(canvas, region);
        for outside in subtract_rect(canvas, ss) {
            painter.rect_filled(outside, CornerRadius::ZERO, Color32::from_black_alpha(120));
        }
        painter.rect_stroke(
            ss,
            CornerRadius::ZERO,
            Stroke::new(1.5, Color32::WHITE),
            StrokeKind::Middle,
        );
        // Handles are only grabbable with the Select tool, so only then are
        // they shown — a painted affordance must work.
        if editor.tool == Tool::Select && !matches!(editor.state, EditorState::RegionDraw { .. }) {
            for (pos, _) in selection_handles(ss) {
                let handle = Rect::from_center_size(pos, Vec2::splat(REGION_HANDLE_SIZE));
                painter.rect_filled(handle, 2.0, Color32::WHITE);
                painter.rect_stroke(
                    handle,
                    2.0,
                    Stroke::new(1.0, Color32::from_black_alpha(180)),
                    StrokeKind::Middle,
                );
            }
            // Move grip, just left of the top-right handle: a plain drag now
            // rubber-band-selects, so the region moves from here instead.
            let grip = region_move_grip(ss);
            painter.circle_filled(grip, REGION_GRIP_RADIUS, ACCENT);
            painter.circle_stroke(grip, REGION_GRIP_RADIUS, Stroke::new(1.5, Color32::WHITE));
            paint_move_icon(painter, grip, 6.0, Color32::WHITE);
        }
        // Export border preview: the outset form grows the output, so it
        // sits *outside* the region rather than over the captured pixels
        // inside it. Drawn at the exported thickness scaled into screen
        // space, so what you see is the proportion you get.
        if editor.border {
            let exported = (
                region.width().round() as u32,
                region.height().round() as u32,
            );
            let w = crate::export::border_width(exported) as f32 * editor.view.zoom;
            if w >= 1.0 {
                painter.rect_stroke(
                    ss.expand(w * 0.5),
                    CornerRadius::ZERO,
                    Stroke::new(w, editor.style.color),
                    StrokeKind::Middle,
                );
            }
        }
        let dims = format!("{}×{}", region.width().round(), region.height().round());
        let (pos, align) = if ss.min.y - canvas.min.y > 24.0 {
            (ss.left_top() + Vec2::new(0.0, -10.0), Align2::LEFT_BOTTOM)
        } else {
            (ss.left_top() + Vec2::new(10.0, 10.0), Align2::LEFT_TOP)
        };
        painter.text(pos, align, dims, FontId::proportional(14.0), Color32::WHITE);
    } else {
        // No region yet: a gentle veil; the toolbar's status line explains
        // what to do.
        painter.rect_filled(canvas, CornerRadius::ZERO, Color32::from_black_alpha(70));
    }
}

/// Full-screen alignment lines at the given x/y coordinates. Each is a
/// white–black–white sandwich so it stands out on any background color.
pub fn paint_crosshairs(painter: &egui::Painter, canvas: Rect, xs: &[f32], ys: &[f32]) {
    let white = Stroke::new(1.0, Color32::from_white_alpha(200));
    let black = Stroke::new(1.0, Color32::from_black_alpha(220));
    for &x in xs {
        for (offset, stroke) in [(-1.0, white), (0.0, black), (1.0, white)] {
            painter.line_segment(
                [Pos2::new(x + offset, canvas.min.y), Pos2::new(x + offset, canvas.max.y)],
                stroke,
            );
        }
    }
    for &y in ys {
        for (offset, stroke) in [(-1.0, white), (0.0, black), (1.0, white)] {
            painter.line_segment(
                [Pos2::new(canvas.min.x, y + offset), Pos2::new(canvas.max.x, y + offset)],
                stroke,
            );
        }
    }
}

/// Small triangle used as an arrowhead for the knob icons.
fn icon_arrowhead(painter: &egui::Painter, tip: Pos2, dir: Vec2, size: f32, color: Color32) {
    let normal = Vec2::new(-dir.y, dir.x) * (size * 0.6);
    let base = tip - dir * size;
    painter.add(EguiShape::convex_polygon(
        vec![tip, base + normal, base - normal],
        color,
        Stroke::NONE,
    ));
}

/// Curved two-headed arrow: the rotate glyph.
fn paint_rotate_icon(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let (start, end) = (-220_f32.to_radians(), 40_f32.to_radians());
    let steps = 16;
    let points: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = start + (end - start) * (i as f32 / steps as f32);
            center + Vec2::new(a.cos(), a.sin()) * radius
        })
        .collect();
    painter.add(EguiShape::line(points, Stroke::new(1.8, color)));
    // Heads tangent to the arc at both ends.
    let tangent = |a: f32| Vec2::new(-a.sin(), a.cos());
    icon_arrowhead(
        painter,
        center + Vec2::new(start.cos(), start.sin()) * radius,
        -tangent(start),
        4.5,
        color,
    );
    icon_arrowhead(
        painter,
        center + Vec2::new(end.cos(), end.sin()) * radius,
        tangent(end),
        4.5,
        color,
    );
}

/// Four-way arrow cross: the move glyph.
fn paint_move_icon(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    let stroke = Stroke::new(1.8, color);
    for dir in [Vec2::RIGHT, Vec2::LEFT, Vec2::DOWN, Vec2::UP] {
        painter.line_segment([center, center + dir * (radius - 2.0)], stroke);
        icon_arrowhead(painter, center + dir * radius, dir, 4.0, color);
    }
}
