//! Inline text editing: a frameless TextEdit rendered at the text's spot,
//! with the same font/color the committed annotation will have. Never
//! soft-wraps — lines break only at typed newlines. A grip pinned to the
//! box drags it, so text can be repositioned without committing first. As
//! the text changes, the annotation is reclamped to the export bounds (the
//! region when there is one, else the image — so it can never be cropped
//! on export) and the view is panned to keep the box on screen (so a long
//! line can't type itself somewhere you can't see).

use eframe::egui::text_selection::CCursorRange;
use eframe::egui::widgets::text_edit::TextEditState;
use eframe::egui::{
    self, Align2, Color32, Context, FontId, Id, Key, Margin, Modifiers, Rect, Sense, TextEdit, Ui,
    Vec2,
};

use crate::editor::Editor;
use crate::editor::state::EditorState;

/// Explicit id for the inline TextEdit, so its cursor can be read back out
/// of egui memory *before* the widget runs this frame.
const INPUT_ID: &str = "text-editor-input";
/// Height of the drag grip, in screen px.
const GRIP_H: f32 = 20.0;
/// Width of the drag grip, in screen px. Fixed rather than matched to the
/// text's width — a large font size made the grip a bar as wide as the box
/// itself, towering over the small drag affordance it actually is.
const GRIP_W: f32 = 40.0;

/// Something the text editor deliberately declined to handle.
pub enum TextEditAction {
    /// Ctrl+C that the editor has no use for: run the app's copy-and-close,
    /// exactly as if no text edit were open.
    CopyAndClose,
}

pub fn show(ctx: &Context, editor: &mut Editor, canvas: Rect) -> Option<TextEditAction> {
    let view = editor.view;
    // Read before the state borrow: what actually gets exported is the
    // *region* crop, not the full image — `render_to_image` draws every
    // annotation onto the full image first and only crops to `region` at
    // the very end. A region is the common case (you drag one out before
    // annotating), so clamping to the full image instead would let text
    // sit safely inside it while still landing outside the region and
    // getting cropped away on save, exactly the bug this is meant to fix.
    // Falls back to the full image only when there's no region yet.
    let export_bounds = editor.doc.region.unwrap_or_else(|| editor.doc.image_rect());
    let mut commit = false;
    let mut cancel = false;
    let EditorState::TextEditing(edit) = &mut editor.state else { return None };

    // Ctrl+C reaches us as a synthetic Copy event (see handle_shortcuts).
    // egui's TextEdit silently drops it when there is nothing selected, so
    // that dead case becomes the app's copy-and-close instead. Decided
    // before the widget is added: afterwards the cursor has already moved
    // and the event is spent. Nothing consumes the event here because the
    // early return skips the TextEdit entirely, and events do not survive
    // the frame.
    let caret = TextEditState::load(ctx, Id::new(INPUT_ID)).and_then(|s| s.cursor.char_range());
    if copy_falls_through(&edit.buffer, caret)
        && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)))
    {
        return Some(TextEditAction::CopyAndClose);
    }

    // Keep the annotation itself from hanging off the export bounds: text
    // always renders with its top-left at `pos` and grows right/down from
    // there, so a long line (or several Shift+Enter'd ones) can reach past
    // them — and whatever's past it is gone, cropped away exactly like
    // anything else outside them. This one *does* move the anchor, unlike
    // the screen-space pan below: keeping it fixed at the original click
    // would mean silently losing the overflowing part of the text forever,
    // which is worse than the text sliding a little while you type.
    //
    // Measured unzoomed, in image px, because that is what export renders
    // in. Export lays out at `pixels_per_point` 1.0 (`export::text::draw`)
    // and this layout runs at the screen's, which in egui 0.36 agree on
    // width exactly and on row height to within half a pixel — `MARGIN`
    // below covers that rounding, so the clamp cannot leave a sliver to be
    // cropped. Rotation is ignored: the inline editor draws unrotated, so
    // this bounds what is actually on screen while typing.
    //
    // Only when the text itself changed, not every frame: the screen pan
    // below would otherwise fight a deliberate scroll-zoom or middle-drag
    // (`canvas::show`) by snapping the box straight back, and re-opening
    // text that already overhangs would move it — recording an undo step
    // for merely looking at it.
    const MARGIN: f32 = 1.0;
    let unzoomed_font = FontId::proportional(edit.style.font_size.max(1.0));
    let text_size_img = ctx
        .fonts_mut(|f| f.layout_no_wrap(edit.buffer.clone(), unzoomed_font, Color32::WHITE))
        .size()
        + Vec2::splat(MARGIN);
    let text_rect_img = Rect::from_min_size(edit.pos, text_size_img);
    let text_changed = edit.last_rect != Some(text_rect_img);
    let img_nudge = if text_changed {
        keep_in_view(text_rect_img, export_bounds)
    } else {
        Vec2::ZERO
    };
    if img_nudge != Vec2::ZERO {
        edit.pos += img_nudge;
    }
    if text_changed {
        edit.last_rect = Some(text_rect_img.translate(img_nudge));
    }

    let screen_pos = view.to_screen(canvas, edit.pos);
    let font = FontId::proportional((edit.style.font_size * view.zoom).max(9.0));
    // Size the editor to its content so clicks next to the text still reach
    // the canvas (and commit); the margin absorbs the one-frame lag of the
    // measurement.
    let text_size = ctx
        .fonts_mut(|f| f.layout_no_wrap(edit.buffer.clone(), font.clone(), Color32::WHITE))
        .size();
    let width = text_size.x + font.size * 2.0;
    let layout_font = font.clone();
    let text_color = edit.style.color;
    let mut layouter = move |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
        ui.fonts_mut(|f| {
            f.layout_no_wrap(buf.as_str().to_owned(), layout_font.clone(), text_color)
        })
    };
    // The clamp above keeps the annotation from reaching past the *export
    // bounds*, but the view can be zoomed/panned so that even a position
    // safely inside them sits outside the visible *canvas* — with nothing
    // watching for that, typing could still run the box off the edge of
    // the screen, with no way to see what you were typing. This time pan
    // the view instead of the anchor: the annotation's position is
    // already settled by the clamp above, and panning brings the box back
    // into view without unsettling it again — the same way typing off the
    // edge of any scrollable text field scrolls the view, not the text.
    // Gated on the same `text_changed` as that clamp, so panning or
    // zooming the box off-canvas on purpose stays put.
    //
    // The grip rides above the box, unless the box sits close enough to
    // the top of the canvas that there is no room for it up there.
    let grip_above = screen_pos.y - GRIP_H >= canvas.min.y;
    let box_min = if grip_above { screen_pos - Vec2::new(0.0, GRIP_H) } else { screen_pos };
    // `text_size` is the laid-out height, not an estimate. The grip is a
    // small fixed-width tab (`GRIP_W`) rather than the box's width, so the
    // text is what governs overflow.
    let box_rect = Rect::from_min_size(
        box_min,
        Vec2::new(width.max(GRIP_W), GRIP_H + text_size.y + MARGIN),
    );
    let nudge = if text_changed {
        keep_in_view(box_rect, canvas)
    } else {
        Vec2::ZERO
    };
    if nudge != Vec2::ZERO {
        // `pan`, not `pan_by`: this is the editor keeping up with itself,
        // not the user taking the view over, so it must not set the
        // `adjusted` flag that disables refitting on a canvas resize.
        editor.view.pan += nudge;
        // `canvas::show` already painted the image this frame at the old
        // pan (see `app::ui`), so the box is drawn where the *old* pan put
        // it and the corrected frame is requested immediately — rather
        // than offsetting the box here, which would leave it misaligned
        // against the image underneath for that frame.
        ctx.request_repaint();
    }
    let area_pos = box_min;
    let mut grip = None;
    egui::Area::new(Id::new("text-editor"))
        .fixed_pos(area_pos)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            // Nothing between grip and box: the TextEdit has to land exactly
            // on screen_pos or it stops lining up with the text it stands in
            // for.
            ui.spacing_mut().item_spacing.y = 0.0;
            if grip_above {
                grip = Some(drag_grip(ui));
            }
            // Plain Enter commits — consumed *before* the TextEdit runs, or
            // the widget first inserts a newline at the cursor and that
            // newline lands in the committed text (trim_end only strips
            // trailing ones). Shift+Enter must fall through to the TextEdit
            // as a line break — but consume_key matches modifiers logically,
            // so a bare Modifiers::NONE pattern swallows Shift+Enter too
            // (extra Shift is ignored). Guard on shift so only unmodified
            // Enter commits.
            if ui.input_mut(|i| !i.modifiers.shift && i.consume_key(Modifiers::NONE, Key::Enter)) {
                commit = true;
            }
            let response = ui.add(
                TextEdit::multiline(&mut edit.buffer)
                    .id(Id::new(INPUT_ID))
                    .font(font)
                    .text_color(edit.style.color)
                    .frame(egui::Frame::NONE)
                    .margin(Margin::ZERO)
                    .desired_rows(1)
                    .desired_width(width)
                    .layouter(&mut layouter),
            );
            if edit.just_created {
                response.request_focus();
                edit.just_created = false;
            }
            if !grip_above {
                grip = Some(drag_grip(ui));
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                cancel = true;
            }
        });
    // Outside the closure: a drag reports screen px, but `pos` is in image
    // coords, so the delta has to come back through the zoom.
    if let Some(grip) = grip {
        if grip.dragged() {
            edit.pos = (edit.pos + grip.drag_delta() / view.zoom)
                .clamp(export_bounds.min, export_bounds.max);
        }
        // Pressing anywhere outside the TextEdit clears egui's focus, which
        // would leave the keyboard nowhere. Hand it back when the drag ends.
        if grip.drag_stopped() {
            ctx.memory_mut(|m| m.request_focus(Id::new(INPUT_ID)));
        }
    }
    if cancel {
        editor.cancel_text();
    } else if commit {
        editor.commit_text();
    }
    None
}

/// The box's drag handle: a small dotted tab, left-aligned above (or below)
/// the box. Drawn with its own fill rather than the egui widget visuals
/// because it sits over captured pixels, which can be any color at all.
fn drag_grip(ui: &mut Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(GRIP_W, GRIP_H), Sense::drag());
    let hot = resp.hovered() || resp.dragged();
    ui.painter().rect_filled(
        rect,
        4.0,
        if hot {
            Color32::from_black_alpha(200)
        } else {
            Color32::from_black_alpha(140)
        },
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        "• • •",
        FontId::proportional(13.0),
        if hot {
            Color32::WHITE
        } else {
            Color32::from_gray(205)
        },
    );
    if resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    resp
}

/// How far `rect` has to move to sit inside `bounds` — the fix for a box
/// that never soft-wraps otherwise growing straight off an edge as you
/// type. Used twice, in both coordinate spaces: image-space, to keep the
/// annotation inside the export bounds, and screen-space, to work out how
/// far to pan the view. Prefers keeping the leading edge (top-left, where
/// reading starts) inside over the trailing edge when `rect` is too big
/// for `bounds` to hold both.
fn keep_in_view(rect: Rect, bounds: Rect) -> Vec2 {
    let mut nudge = Vec2::ZERO;
    if rect.right() > bounds.max.x {
        nudge.x = bounds.max.x - rect.right();
    }
    if rect.left() + nudge.x < bounds.min.x {
        nudge.x = bounds.min.x - rect.left();
    }
    if rect.bottom() > bounds.max.y {
        nudge.y = bounds.max.y - rect.bottom();
    }
    if rect.top() + nudge.y < bounds.min.y {
        nudge.y = bounds.min.y - rect.top();
    }
    nudge
}

/// Does Ctrl+C belong to the app rather than to the text editor? Only when
/// the editor would do nothing with it: nothing selected, caret at the end.
/// Anywhere else it stays a text operation — mid-buffer it is the caret the
/// user would grow a selection from, so stealing it would surprise.
///
/// A just-opened editor has no stored cursor yet; its buffer is empty, so
/// the caret is trivially at the end.
fn copy_falls_through(buffer: &str, caret: Option<CCursorRange>) -> bool {
    caret.is_none_or(|r| r.is_empty() && usize::from(r.primary.index) >= buffer.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Pos2;
    use eframe::egui::text::CCursor;

    fn caret_at(index: usize) -> Option<CCursorRange> {
        Some(CCursorRange::one(CCursor::new(index)))
    }

    #[test]
    fn copy_falls_through_only_at_the_end_without_a_selection() {
        assert!(copy_falls_through("hello", caret_at(5)));
        assert!(copy_falls_through("", caret_at(0)));
        // Mid-buffer: the editor's own caret, leave it alone.
        assert!(!copy_falls_through("hello", caret_at(4)));
        assert!(!copy_falls_through("hello", caret_at(0)));
        // A selection is always the editor's, even one ending at the end.
        assert!(!copy_falls_through(
            "hello",
            Some(CCursorRange::two(CCursor::new(0), CCursor::new(5)))
        ));
    }

    #[test]
    fn a_fresh_editor_has_no_cursor_yet() {
        assert!(copy_falls_through("", None));
    }

    #[test]
    fn the_end_is_counted_in_chars_not_bytes() {
        // 5 chars, 10 bytes: a byte-indexed comparison would never match.
        assert!(copy_falls_through("héllö", caret_at(5)));
        assert!(!copy_falls_through("héllö", caret_at(4)));
    }

    #[test]
    fn a_box_fully_inside_the_canvas_needs_no_nudge() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let box_rect = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(200.0, 40.0));
        assert_eq!(keep_in_view(box_rect, canvas), Vec2::ZERO);
    }

    #[test]
    fn a_line_growing_past_the_right_edge_pans_left_to_fit_it() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        // Anchored at x=900, 300 wide: runs 200px past the right edge.
        let box_rect = Rect::from_min_size(Pos2::new(900.0, 100.0), Vec2::new(300.0, 40.0));
        let nudge = keep_in_view(box_rect, canvas);
        assert_eq!(nudge, Vec2::new(-200.0, 0.0));
        assert_eq!((box_rect.min + nudge).x + box_rect.width(), canvas.max.x);
    }

    #[test]
    fn a_tall_multiline_box_pans_up_to_fit_the_bottom_edge() {
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let box_rect = Rect::from_min_size(Pos2::new(100.0, 700.0), Vec2::new(200.0, 150.0));
        let nudge = keep_in_view(box_rect, canvas);
        assert_eq!(nudge, Vec2::new(0.0, -50.0));
    }

    #[test]
    fn a_box_wider_than_the_canvas_keeps_its_leading_edge_visible() {
        // 1200 wide box, 1000-wide canvas: it can never fully fit, so the
        // left (leading, reading-start) edge wins over the right.
        let canvas = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let box_rect = Rect::from_min_size(Pos2::new(50.0, 100.0), Vec2::new(1200.0, 40.0));
        let nudge = keep_in_view(box_rect, canvas);
        assert_eq!((box_rect.min + nudge).x, canvas.min.x);
    }

    #[test]
    fn the_region_is_what_text_is_kept_inside_not_the_whole_image() {
        // The region is what export crops to, so a position comfortably
        // inside the image can still be cropped away. 200 wide at x=900
        // fits the 1000-wide image but overhangs a region ending at 800.
        let image = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let region = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(800.0, 700.0));
        let text = Rect::from_min_size(Pos2::new(900.0, 200.0), Vec2::new(200.0, 40.0));
        assert_eq!(keep_in_view(text, image), Vec2::new(-100.0, 0.0));
        assert_eq!(keep_in_view(text, region), Vec2::new(-300.0, 0.0));
        // Clamped to the region, the text ends exactly on its edge.
        assert_eq!(
            (text.min + keep_in_view(text, region)).x + text.width(),
            region.max.x
        );
    }

    #[test]
    fn a_rect_already_inside_its_bounds_is_left_alone() {
        // The gate that stops this running every frame relies on an
        // in-bounds rect asking for nothing, so a repeat measurement of
        // unchanged text can't nudge the view.
        let bounds = Rect::from_min_max(Pos2::new(100.0, 100.0), Pos2::new(800.0, 700.0));
        for pos in [
            Pos2::new(100.0, 100.0),
            Pos2::new(400.0, 400.0),
            Pos2::new(600.0, 600.0),
        ] {
            let rect = Rect::from_min_size(pos, Vec2::new(200.0, 40.0));
            assert_eq!(keep_in_view(rect, bounds), Vec2::ZERO, "{pos:?}");
        }
    }
}
