//! The always-visible toolbar: tools two per row, the style controls, the
//! action buttons, and — at its foot — the status line explaining what the
//! current editor state affords (or demanding attention, e.g. the discard
//! confirmation). Auto-placed beside the region, draggable by its grip.

use eframe::egui::{
    self, Align2, Button, Color32, Context, FontId, Id, Key, KeyboardShortcut, Modifiers, Pos2,
    Rect, RichText, Sense, Slider, Stroke, StrokeKind, Vec2,
};

use crate::annotate::Tool;
use crate::editor::{Editor, StatusKind};
use crate::ui::{
    ACCENT, ACTIVE_TOOL_FILL, BTN_H, TOOLBAR_W, TOOLS, UiScale, about::About, color_picker,
};

/// Actions the toolbar can't perform itself (they need export/clipboard/
/// viewport access); the app layer executes them.
pub enum ToolbarAction {
    Copy {
        close: bool,
    },
    Save {
        close: bool,
    },
    Close,
    #[cfg(any(windows, target_os = "macos"))]
    OpenImage,
    #[cfg(any(windows, target_os = "macos"))]
    ChooseOutputDirectory,
}

/// An app-level message that overrides the editor's status line (save/copy
/// results and failures).
pub struct StatusOverride {
    pub message: String,
    pub is_error: bool,
}

pub struct Toolbar {
    /// Where the user dragged the toolbar; `None` = auto beside the region.
    pos: Option<Pos2>,
    /// Rect measured last frame, for the auto placement.
    size: Vec2,
    /// User-chosen toolbar density; persisted via `prefs`.
    pub ui_scale: UiScale,
    /// Tallest the status line has needed to be so far this session, so it
    /// can only grow — never shrink back down and make the panel jump.
    status_min_h: f32,
    /// The scale factor `status_min_h` was measured at. It is a height in
    /// screen px at one font size and padding, so a scale change makes it
    /// meaningless: it is remeasured from zero the first frame at the new
    /// scale (otherwise shrinking Large → Small would leave the footer at
    /// the Large height until a longer message happened to grow it).
    status_min_h_scale: f32,
    about: About,
}

impl Toolbar {
    pub fn new(ui_scale: UiScale) -> Self {
        // Only the first frame's auto-placement reads this; the measured
        // rect replaces it after that.
        let w = TOOLBAR_W * ui_scale.factor();
        Self {
            pos: None,
            size: Vec2::new(w + 30.0, 500.0),
            ui_scale,
            status_min_h: 0.0,
            status_min_h_scale: ui_scale.factor(),
            about: About::default(),
        }
    }

    pub fn show(
        &mut self,
        ctx: &Context,
        editor: &mut Editor,
        canvas: Rect,
        status_override: Option<StatusOverride>,
    ) -> Option<ToolbarAction> {
        let mut action = self.show_file_actions(ctx, canvas);
        self.about.show(ctx, canvas);
        // Keep initial selection unobstructed, but never hide errors or
        // the discard confirmation just because there is no region yet.
        let Some(ss) = editor
            .doc
            .region
            .map(|r| editor.view.rect_to_screen(canvas, r))
        else {
            editor.color_picker = None;
            if status_override.is_some() || editor.state.is_confirm_discard() {
                let scale = self.ui_scale.factor();
                let status = status_line(editor, status_override.as_ref());
                egui::Area::new(Id::new("selection-feedback"))
                    .anchor(Align2::CENTER_BOTTOM, Vec2::new(0.0, -12.0))
                    .order(egui::Order::Foreground)
                    .constrain_to(canvas)
                    .show(ctx, |ui| {
                        ui.set_width((TOOLBAR_W * scale).min((canvas.width() - 40.0).max(1.0)));
                        show_status(ui, &status, scale, 0.0, canvas.height() * 0.4);
                    });
            }
            return action;
        };
        // Keep desktop file actions above the toolbar, including when the
        // selected region begins at the top-left corner of the screen.
        let toolbar_canvas = if cfg!(any(windows, target_os = "macos")) {
            Rect::from_min_max(canvas.min + Vec2::new(0.0, 48.0), canvas.max)
        } else {
            canvas
        };
        let status = status_line(editor, status_override.as_ref());
        let margin = 12.0;
        let size = self.size;
        let default_pos = {
            let x = if ss.min.x - size.x - margin >= canvas.min.x {
                ss.min.x - size.x - margin
            } else if ss.max.x + size.x + margin <= canvas.max.x {
                ss.max.x + margin
            } else {
                ss.min.x + margin
            };
            Pos2::new(x, ss.min.y)
        };
        let lo = toolbar_canvas.min;
        let hi = (toolbar_canvas.max - size).max(lo);
        let pos = self.pos.unwrap_or(default_pos).clamp(lo, hi);
        // Every size in the panel derives from this one factor, so Small,
        // Medium, and Large stay proportional instead of drifting apart.
        let scale = self.ui_scale.factor();
        let toolbar_w = (TOOLBAR_W * scale).round();
        let btn_h = (BTN_H * scale).round();
        let btn_font = (15.0 * scale).round().max(10.0);
        let area = egui::Area::new(Id::new("toolbar"))
            .fixed_pos(pos)
            .order(egui::Order::Foreground)
            .constrain_to(toolbar_canvas)
            .show(ctx, |ui| {
                let frame = egui::Frame::popup(ui.style());
                let content_size =
                    toolbar_canvas.size() - frame.total_margin().sum() - Vec2::splat(4.0);
                frame.show(ui, |ui| {
                    let toolbar_w = toolbar_w.min(content_size.x.max(1.0));
                    ui.set_width(toolbar_w);
                    // Chunky, easy-to-hit controls that stand out from the
                    // popup background.
                    // `min_size(interact_size)` (what DragValue sets its
                    // box to) is a floor, not a cap — the same trap the
                    // size-picker buttons hit: three digits ("120", the
                    // top of the text-size range) at a big scale need more
                    // than the box's nominal width, and since nothing
                    // clips, the box — and with it the row, and the popup
                    // Frame sized to fit its content — grows past
                    // `toolbar_w` instead. Size it with real headroom
                    // above the widest value this range ever shows, not
                    // tight against it, and share it with `slider_width`
                    // below so the row math stays exact regardless.
                    let interact_w = 54.0 * scale;
                    let gap_w = 6.0 * scale;
                    let spacing = ui.spacing_mut();
                    spacing.button_padding = Vec2::new(8.0, 6.0) * scale;
                    spacing.item_spacing = Vec2::new(gap_w, gap_w);
                    spacing.interact_size = Vec2::new(interact_w, btn_h);
                    let visuals = ui.visuals_mut();
                    visuals.widgets.inactive.weak_bg_fill = Color32::from_gray(58);
                    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, Color32::from_gray(235));
                    visuals.widgets.hovered.weak_bg_fill = Color32::from_gray(80);
                    visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, Color32::WHITE);
                    visuals.widgets.active.weak_bg_fill = Color32::from_gray(96);
                    let styles = &mut ui.style_mut().text_styles;
                    if let Some(font) = styles.get_mut(&egui::TextStyle::Button) {
                        font.size = btn_font;
                    }
                    if let Some(font) = styles.get_mut(&egui::TextStyle::Body) {
                        font.size = btn_font;
                    }
                    let content_top = ui.cursor().top();
                    let gap = ui.spacing().item_spacing.x;
                    let (grip_rect, grip) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 22.0 * scale),
                        Sense::drag(),
                    );
                    ui.painter().text(
                        grip_rect.center(),
                        Align2::CENTER_CENTER,
                        "• • •",
                        FontId::proportional((14.0 * scale).round().max(10.0)),
                        ui.visuals().weak_text_color(),
                    );
                    if grip.hovered() || grip.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    if grip.dragged() {
                        self.pos = Some(pos + grip.drag_delta());
                    }
                    ui.separator();

                    // Toolbar density: three small buttons, each an "A"
                    // drawn at that option's own relative size, so the row
                    // previews the effect directly instead of naming it. No
                    // heading, and — unlike every grid row below — not
                    // stretched to the panel's full width: this is a rarely
                    // -touched setting, not a primary action, so it sits
                    // small and tucked to the right instead of claiming the
                    // same visual weight as Undo or Copy.
                    // Pinned to an exact height and laid out right-to-left,
                    // rather than a plain `ui.horizontal` nudged over with
                    // `add_space`: a bare horizontal layout reserves at
                    // least `spacing().interact_size.y` (still the big
                    // 32px-ish tool-button height at this point) for its
                    // row even when its content is shorter, which is what
                    // left these buttons sitting in the lower half of a
                    // taller-than-they-are band instead of flush under the
                    // separator above.
                    let small_h = (20.0 * scale).round();
                    ui.allocate_ui_with_layout(
                        Vec2::new(ui.available_width(), small_h),
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            let btn_w = (24.0 * scale).round();
                            // `add_sized`'s size is a floor, not a cap —
                            // Button's own min_size.y is unioned with
                            // `interact_size.y` (harmless, pinned to
                            // `small_h` below) but its *content* height
                            // (glyph + button_padding) is never clamped
                            // down to fit. With button_padding untouched
                            // and the "Large" glyph deliberately drawn
                            // bigger, that button alone came out ~27px
                            // tall against a 20px target. Zero the padding
                            // here so content height is the glyph alone,
                            // and scale that glyph by *both* the chosen
                            // option's own relative size and the active
                            // toolbar scale, so it can't exceed `small_h`.
                            ui.spacing_mut().interact_size = Vec2::new(btn_w, small_h);
                            ui.spacing_mut().button_padding = Vec2::ZERO;
                            for opt in UiScale::ALL.into_iter().rev() {
                                let active = self.ui_scale == opt;
                                let text = RichText::new("A")
                                    .size((14.0 * opt.factor() * scale).round().max(8.0))
                                    .color(if active {
                                        Color32::WHITE
                                    } else {
                                        Color32::from_gray(235)
                                    });
                                let mut btn = Button::new(text);
                                if active {
                                    btn = btn.fill(ACTIVE_TOOL_FILL);
                                }
                                let resp = ui
                                    .add_sized(Vec2::new(btn_w, small_h), btn)
                                    .on_hover_text(opt.name());
                                if resp.clicked() {
                                    resp.surrender_focus();
                                    if self.ui_scale != opt {
                                        self.ui_scale = opt;
                                        // The panel is about to change size,
                                        // but `pos`'s clamp this frame was
                                        // already computed from last frame's
                                        // (now-stale) `self.size` — a
                                        // toolbar docked near the canvas
                                        // edge could render past it for this
                                        // one frame. Ask for an immediate
                                        // repaint so the next frame's
                                        // correctly-measured clamp lands
                                        // right away, instead of waiting on
                                        // whatever future input happens to
                                        // trigger one. (The status line's
                                        // high-water mark resets itself
                                        // on that frame, keyed on the
                                        // scale it was measured at — see
                                        // `status_min_h_scale`.)
                                        ctx.request_repaint();
                                    }
                                }
                            }
                        },
                    );
                    ui.separator();

                    // Reserve the wrapped status footer before assigning the rest
                    // to the scroll area. This keeps feedback visible at high DPI
                    // while every tool and action remains reachable by scrolling.
                    // The footer's height only ever grows to fit the longest
                    // message shown so far this session (capped so a very long
                    // error scrolls rather than starving the controls), so
                    // switching between a one-line hint and a wrapped one
                    // can't make the whole panel jump.
                    let status_max_h = content_size.y * 0.3;
                    let status_pad = (8.0 * scale).round();
                    let status_text_h = ui
                        .painter()
                        .layout(
                            status.message.clone(),
                            FontId::proportional(status_font(scale)),
                            status.fg,
                            (ui.available_width() - 2.0 * status_pad).max(1.0),
                        )
                        .size()
                        .y
                        .min(status_max_h);
                    if self.status_min_h_scale != scale {
                        self.status_min_h = 0.0;
                        self.status_min_h_scale = scale;
                    }
                    self.status_min_h = self.status_min_h.max(status_text_h);
                    let status_height = self.status_min_h + status_pad * 2.0;
                    let body_height = (content_size.y
                        - (ui.cursor().top() - content_top)
                        - status_height
                        - ui.spacing().item_spacing.y * 3.0
                        - 8.0)
                        .max(1.0);
                    // A bar only when there is something to scroll. egui's
                    // default scroll style is *floating*: it reserves no
                    // width and draws over the content's right edge, and
                    // `AlwaysVisible` overrides the fade that would
                    // otherwise hide it while dormant — so on a canvas tall
                    // enough to need no scrolling at all, a 10px bar still
                    // sat permanently across the right-hand ends of the
                    // tool and action buttons.
                    //
                    // `VisibleWhenNeeded` alone would swap that for the
                    // opposite problem, since a dormant floating bar is
                    // fully transparent: the panel would be scrollable with
                    // nothing to say so. A solid bar reserves its own width
                    // instead, so when it does appear it sits beside the
                    // buttons rather than on top of them, and the controls
                    // reflow into what's left.
                    ui.spacing_mut().scroll = egui::style::ScrollStyle {
                        bar_width: (6.0 * scale).round(),
                        ..egui::style::ScrollStyle::solid()
                    };
                    egui::ScrollArea::vertical()
                        .id_salt("toolbar-controls")
                        .max_height(body_height)
                        .auto_shrink([false, true])
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                        )
                        .show(ui, |ui| {
                            // Rail, less the drag-value box egui puts beside it.
                            ui.spacing_mut().slider_width =
                                (ui.available_width() - interact_w - gap_w).max(20.0);
                            // Tools, two per row.
                            let half = (ui.available_width() - gap) / 2.0;
                            for pair in TOOLS.chunks(2) {
                                ui.horizontal(|ui| {
                                    for (tool, key) in pair {
                                        let active = editor.tool == *tool;
                                        let text =
                                            RichText::new(format!("{key:?} · {}", tool.label()))
                                                .size(btn_font)
                                                .color(if active {
                                                    Color32::WHITE
                                                } else {
                                                    Color32::from_gray(235)
                                                });
                                        let mut btn = Button::new(text);
                                        if active {
                                            btn = btn.fill(ACTIVE_TOOL_FILL);
                                        }
                                        let resp = ui.add_sized(Vec2::new(half, btn_h), btn);
                                        if resp.clicked() {
                                            resp.surrender_focus();
                                            editor.set_tool(*tool);
                                        }
                                    }
                                });
                            }
                            ui.separator();

                            // Settings target: text being edited, the selection, or
                            // the defaults new objects will use. The controls display
                            // and edit the target's actual values.
                            let editing_text = editor.state.is_text_editing();
                            let selection_target =
                                editor.tool == Tool::Select && !editor.selected.is_empty();
                            let shown_style = editor.shown_style();
                            ui.vertical_centered(|ui| {
                                if editing_text || (selection_target && editor.selected.len() == 1)
                                {
                                    ui.label(
                                        RichText::new("For Current Object").color(ACCENT).strong(),
                                    );
                                } else if selection_target {
                                    ui.label(
                                        RichText::new(format!(
                                            "For {} Selected",
                                            editor.selected.len()
                                        ))
                                        .color(ACCENT)
                                        .strong(),
                                    );
                                } else {
                                    ui.label(RichText::new("For New Objects").weak());
                                }
                            });
                            // Current color; click to open the picker.
                            let (swatch_rect, swatch) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 30.0 * scale),
                                Sense::click(),
                            );
                            ui.painter()
                                .rect_filled(swatch_rect, 4.0, shown_style.color);
                            ui.painter().rect_stroke(
                                swatch_rect,
                                4.0,
                                Stroke::new(1.0, Color32::from_gray(110)),
                                StrokeKind::Middle,
                            );
                            let [r, g, b, _] = shown_style.color.to_srgba_unmultiplied();
                            let luma =
                                0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
                            ui.painter().text(
                                swatch_rect.center(),
                                Align2::CENTER_CENTER,
                                "Color…",
                                FontId::proportional(btn_font),
                                if luma > 140.0 {
                                    Color32::BLACK
                                } else {
                                    Color32::WHITE
                                },
                            );
                            if swatch.clicked() {
                                editor.color_picker = Some(shown_style.color);
                            }
                            color_picker::show(ctx, editor, canvas, swatch_rect, scale);
                            ui.label("Width");
                            let mut width_val = shown_style.width;
                            let mut size_val = shown_style.font_size;
                            let width_slider =
                                ui.add(Slider::new(&mut width_val, 1.0..=24.0).fixed_decimals(0));
                            ui.label("Text size");
                            let size_slider =
                                ui.add(Slider::new(&mut size_val, 8.0..=120.0).fixed_decimals(0));
                            // Only the property the user moved is applied, so one
                            // slider can't homogenize the other across a selection.
                            if width_slider.changed() || size_slider.changed() {
                                editor.adjust_style(
                                    width_slider.changed().then_some(width_val),
                                    size_slider.changed().then_some(size_val),
                                );
                            } else if !width_slider.dragged() && !size_slider.dragged() {
                                editor.end_style_adjust();
                            }
                            // Sliders hold keyboard focus after use, which would
                            // silently eat every shortcut (S, Space, Esc, Del…).
                            if width_slider.drag_stopped() {
                                width_slider.surrender_focus();
                            }
                            if size_slider.drag_stopped() {
                                size_slider.surrender_focus();
                            }
                            ui.separator();

                            // Paired action buttons. `add_sized` allocates the rect
                            // before laying the label out inside it, so a long label
                            // can no longer push its button past its half of the row
                            // the way `min_size` (a floor, not a width) allowed.
                            // `accent` marks the row's default action (the one that
                            // closes the window) with the same blue as an active
                            // tool, so it reads as the recommended button at a
                            // glance instead of blending into Undo/Redo/Reset.
                            let pair = |ui: &mut egui::Ui,
                                        a: (&str, bool, bool),
                                        b: (&str, bool, bool)|
                             -> (bool, bool) {
                                let mut clicked = (false, false);
                                ui.horizontal(|ui| {
                                    for (i, (label, enabled, accent)) in
                                        [a, b].into_iter().enumerate()
                                    {
                                        let mut btn = Button::new(label);
                                        if accent {
                                            btn = btn.fill(ACTIVE_TOOL_FILL);
                                        }
                                        let btn = ui
                                            .add_enabled_ui(enabled, |ui| {
                                                ui.add_sized(Vec2::new(half, btn_h), btn)
                                            })
                                            .inner;
                                        if btn.clicked() {
                                            btn.surrender_focus();
                                            if i == 0 {
                                                clicked.0 = true;
                                            } else {
                                                clicked.1 = true;
                                            }
                                        }
                                    }
                                });
                                clicked
                            };
                            let shortcut = |key| {
                                ctx.format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key))
                            };
                            let undo_label = format!("Undo {}", shortcut(Key::Z));
                            let redo_label = format!("Redo {}", shortcut(Key::Y));
                            let (undo, redo) = pair(
                                ui,
                                (&undo_label, editor.doc.can_undo(), false),
                                (&redo_label, editor.doc.can_redo(), false),
                            );
                            if undo {
                                editor.undo();
                            }
                            if redo {
                                editor.redo();
                            }
                            let (fit, reset) = pair(
                                ui,
                                ("Reset view F", true, false),
                                ("Reset all", true, false),
                            );
                            if fit {
                                editor.view.fit(editor.doc.image_size(), canvas.size());
                            }
                            if reset {
                                editor.reset_all();
                            }
                            ui.separator();
                            let copy_label = format!("Copy+Close {}", shortcut(Key::C));
                            let (copy, copy_close) =
                                pair(ui, ("Copy", true, false), (&copy_label, true, true));
                            if copy {
                                action = Some(ToolbarAction::Copy { close: false });
                            }
                            if copy_close {
                                action = Some(ToolbarAction::Copy { close: true });
                            }
                            let save_label = format!("Save+Close {}", shortcut(Key::S));
                            let (save, save_close) =
                                pair(ui, ("Save", true, false), (&save_label, true, true));
                            if save {
                                action = Some(ToolbarAction::Save { close: false });
                            }
                            if save_close {
                                action = Some(ToolbarAction::Save { close: true });
                            }
                            // Spans the popup instead of shrink-wrapping its label.
                            if ui
                                .add_sized(
                                    Vec2::new(ui.available_width(), btn_h),
                                    Button::new("Close Esc"),
                                )
                                .clicked()
                            {
                                action = Some(ToolbarAction::Close);
                            }
                        });
                    ui.separator();
                    show_status(ui, &status, scale, self.status_min_h, status_max_h);
                });
            });
        self.size = area.response.rect.size();
        action
    }

    fn show_file_actions(&self, ctx: &Context, canvas: Rect) -> Option<ToolbarAction> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            let mut action = None;
            egui::Area::new(Id::new("native-file-actions"))
                .anchor(Align2::LEFT_TOP, Vec2::new(12.0, 12.0))
                .constrain_to(canvas)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let open = ui.button("Open PNG…");
                        if open.clicked() {
                            open.surrender_focus();
                            action = Some(ToolbarAction::OpenImage);
                        }
                        let folder = ui.button("Save folder…");
                        if folder.clicked() {
                            folder.surrender_focus();
                            action = Some(ToolbarAction::ChooseOutputDirectory);
                        }
                    });
                });
            action
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (ctx, canvas);
            None
        }
    }
}

/// What the status footer shows: only a genuine alert (the discard
/// confirmation) or an error gets a filled, colored callout — a plain hint
/// or success message renders as bare text so it can't be mistaken for a
/// (non-clickable) button.
struct StatusLine {
    fill: Option<Color32>,
    fg: Color32,
    message: String,
}

/// App-level feedback takes priority over the state hint.
fn status_line(editor: &Editor, status: Option<&StatusOverride>) -> StatusLine {
    let (fill, fg, message) = match status {
        Some(over) if over.is_error => (
            Some(Color32::from_rgb(0x7a, 0x1d, 0x1d)),
            Color32::WHITE,
            over.message.clone(),
        ),
        Some(over) => (None, Color32::from_gray(220), over.message.clone()),
        None => match editor.status() {
            (StatusKind::Alert, message) => (
                Some(Color32::from_rgb(0xf2, 0xd0, 0x2e)),
                Color32::BLACK,
                message,
            ),
            (StatusKind::Hint, message) => (None, Color32::from_gray(210), message),
        },
    };
    StatusLine { fill, fg, message }
}

fn status_font(scale: f32) -> f32 {
    (14.0 * scale).round().max(11.0)
}

/// `min_h` reserves at least that much text height (the session's
/// high-water mark) so the footer never shrinks; `max_h` caps it so a very
/// long message scrolls instead of crowding out the controls.
fn show_status(ui: &mut egui::Ui, status: &StatusLine, scale: f32, min_h: f32, max_h: f32) {
    let pad = (8.0 * scale).round();
    egui::Frame::default()
        .fill(status.fill.unwrap_or(Color32::TRANSPARENT))
        .corner_radius(if status.fill.is_some() { 4.0 } else { 0.0 })
        .inner_margin(pad)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.set_min_height(min_h.min(max_h).max(0.0));
            egui::ScrollArea::vertical()
                .id_salt("status-message")
                .max_height(max_h.max(1.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(status.message.clone())
                            .color(status.fg)
                            .size(status_font(scale)),
                    );
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{annotate::Style, document::Document, editor::state::EditorState};
    use egui::{Event, FullOutput, MouseWheelUnit, PointerButton, RawInput, Shape, ViewportId};
    use image::RgbaImage;

    fn canvas() -> Rect {
        // 1920×1080 pixels at 200% scaling.
        Rect::from_min_size(Pos2::ZERO, Vec2::new(960.0, 540.0))
    }

    fn editor() -> Editor {
        let mut editor = Editor::new(
            Document::new(RgbaImage::new(1920, 1080)),
            Style {
                color: Color32::RED,
                width: 4.0,
                font_size: 24.0,
            },
            vec![Color32::RED],
            false,
        );
        editor.view.fit(editor.doc.image_size(), canvas().size());
        editor
    }

    fn frame(
        ctx: &Context,
        toolbar: &mut Toolbar,
        editor: &mut Editor,
        events: Vec<Event>,
        status: Option<StatusOverride>,
    ) -> (FullOutput, Option<ToolbarAction>) {
        let mut input = RawInput {
            screen_rect: Some(canvas()),
            events,
            ..Default::default()
        };
        input
            .viewports
            .get_mut(&ViewportId::ROOT)
            .unwrap()
            .native_pixels_per_point = Some(2.0);
        let mut action = None;
        let output = crate::test_support::run_ui(ctx, input, |ui| {
            let ctx = ui.ctx();
            action = toolbar.show(
                ctx,
                editor,
                canvas(),
                status.as_ref().map(|s| StatusOverride {
                    message: s.message.clone(),
                    is_error: s.is_error,
                }),
            );
        });
        (output, action)
    }

    fn visible_text(output: &FullOutput, wanted: &str) -> Option<Rect> {
        output.shapes.iter().find_map(|clipped| {
            let Shape::Text(text) = &clipped.shape else {
                return None;
            };
            let rect = text.visual_bounding_rect();
            (text.galley.job.text == wanted
                && canvas().contains_rect(rect)
                && clipped.clip_rect.contains_rect(rect))
            .then_some(rect)
        })
    }

    #[test]
    fn toolbar_fits_high_dpi_canvas_and_scrolling_reaches_save() {
        let ctx = Context::default();
        let mut toolbar = Toolbar::new(UiScale::default());
        let mut editor = editor();
        editor.doc.region = Some(editor.doc.image_rect());
        for _ in 0..3 {
            frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        }
        let rect = ctx.memory(|m| m.area_rect(Id::new("toolbar"))).unwrap();
        assert!(canvas().contains_rect(rect), "toolbar overflows: {rect:?}");
        let status = editor.status().1;
        let (output, _) = frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        assert!(
            visible_text(&output, &status).is_some(),
            "status footer is clipped"
        );
        assert!(
            visible_text(&output, "A · Arrow").is_some(),
            "first tools are reachable"
        );

        // Scroll the real widget using pointer/wheel input, then click the
        // previously inaccessible Save button and verify the dispatched action.
        let pointer = rect.center();
        frame(
            &ctx,
            &mut toolbar,
            &mut editor,
            vec![
                Event::PointerMoved(pointer),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    phase: egui::TouchPhase::Move,
                    delta: Vec2::new(0.0, -2000.0),
                    modifiers: Modifiers::NONE,
                },
            ],
            None,
        );
        let mut output = None;
        for _ in 0..30 {
            output = Some(frame(&ctx, &mut toolbar, &mut editor, vec![], None).0);
        }
        let output = output.unwrap();
        assert!(
            visible_text(&output, &status).is_some(),
            "scroll moved the status footer"
        );
        assert!(
            visible_text(&output, "Close Esc").is_some(),
            "last action is unreachable"
        );
        let save = visible_text(&output, "Save").expect("Save must be visible after scrolling");
        let (_, action) = frame(
            &ctx,
            &mut toolbar,
            &mut editor,
            vec![
                Event::PointerMoved(save.center()),
                Event::PointerButton {
                    pos: save.center(),
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
                Event::PointerButton {
                    pos: save.center(),
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            None,
        );
        assert!(matches!(action, Some(ToolbarAction::Save { close: false })));
    }

    #[test]
    fn status_footer_remeasures_when_scale_shrinks() {
        let ctx = Context::default();
        let mut toolbar = Toolbar::new(UiScale::Large);
        let mut editor = editor();
        editor.doc.region = Some(editor.doc.image_rect());
        let long = Some(StatusOverride {
            message: "Saved to a rather long path that wraps onto several lines of the footer"
                .repeat(2),
            is_error: false,
        });
        for _ in 0..3 {
            frame(
                &ctx,
                &mut toolbar,
                &mut editor,
                vec![],
                long.as_ref().map(clone_status),
            );
        }
        let at_large = toolbar.status_min_h;
        assert!(at_large > 0.0);

        toolbar.ui_scale = UiScale::Small;
        for _ in 0..3 {
            frame(
                &ctx,
                &mut toolbar,
                &mut editor,
                vec![],
                long.as_ref().map(clone_status),
            );
        }
        let at_small = toolbar.status_min_h;
        assert!(
            at_small < at_large,
            "footer kept the Large height after shrinking: {at_small} vs {at_large}"
        );

        // A fresh Small toolbar measures the same message identically, so
        // the reset really remeasured rather than merely shrinking a bit.
        let mut fresh = Toolbar::new(UiScale::Small);
        for _ in 0..3 {
            frame(
                &ctx,
                &mut fresh,
                &mut editor,
                vec![],
                long.as_ref().map(clone_status),
            );
        }
        assert_eq!(fresh.status_min_h, at_small);
    }

    fn clone_status(s: &StatusOverride) -> StatusOverride {
        StatusOverride {
            message: s.message.clone(),
            is_error: s.is_error,
        }
    }

    #[test]
    fn color_picker_keeps_confirmation_visible_on_high_dpi_canvas() {
        let ctx = Context::default();
        let mut toolbar = Toolbar::new(UiScale::default());
        let mut editor = editor();
        editor.doc.region = Some(editor.doc.image_rect());
        editor.color_picker = Some(Color32::RED);
        for _ in 0..3 {
            frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        }
        let rect = ctx
            .memory(|m| m.area_rect(Id::new("custom-color-picker")))
            .unwrap();
        assert!(
            canvas().contains_rect(rect),
            "color picker overflows: {rect:?}"
        );
        let (output, _) = frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        assert!(
            visible_text(&output, "OK").is_some(),
            "confirmation is clipped"
        );
        assert!(
            visible_text(&output, "Cancel").is_some(),
            "cancellation is clipped"
        );
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn native_file_actions_are_available_before_region_selection() {
        let ctx = Context::default();
        let mut toolbar = Toolbar::new(UiScale::default());
        let mut editor = editor();
        for _ in 0..3 {
            frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        }
        let (output, _) = frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        let open = visible_text(&output, "Open PNG…").expect("Open is available without a region");
        assert!(visible_text(&output, "Save folder…").is_some());
        let (_, action) = frame(
            &ctx,
            &mut toolbar,
            &mut editor,
            vec![
                Event::PointerMoved(open.center()),
                Event::PointerButton {
                    pos: open.center(),
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                },
                Event::PointerButton {
                    pos: open.center(),
                    button: PointerButton::Primary,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                },
            ],
            None,
        );
        assert!(matches!(action, Some(ToolbarAction::OpenImage)));
        editor.doc.region = Some(editor.doc.image_rect());
        for _ in 0..3 {
            frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        }
        let toolbar_rect = ctx.memory(|m| m.area_rect(Id::new("toolbar"))).unwrap();
        let files_rect = ctx
            .memory(|m| m.area_rect(Id::new("native-file-actions")))
            .unwrap();
        assert!(
            !toolbar_rect.intersects(files_rect),
            "toolbar must not cover file actions"
        );
    }

    #[test]
    fn errors_and_discard_confirmation_are_visible_without_region() {
        let ctx = Context::default();
        let mut toolbar = Toolbar::new(UiScale::default());
        let mut editor = editor();
        for _ in 0..3 {
            frame(
                &ctx,
                &mut toolbar,
                &mut editor,
                vec![],
                Some(StatusOverride {
                    message: "Could not save screenshot".to_owned(),
                    is_error: true,
                }),
            );
        }
        let (output, _) = frame(
            &ctx,
            &mut toolbar,
            &mut editor,
            vec![],
            Some(StatusOverride {
                message: "Could not save screenshot".to_owned(),
                is_error: true,
            }),
        );
        assert!(visible_text(&output, "Could not save screenshot").is_some());
        assert!(visible_text(&output, "About & Privacy").is_some());
        editor.state = EditorState::ConfirmDiscard { until: 10.0 };
        let (output, _) = frame(&ctx, &mut toolbar, &mut editor, vec![], None);
        assert!(visible_text(&output, "Esc again to discard and close").is_some());
    }
}
