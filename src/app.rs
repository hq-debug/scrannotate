//! The eframe shell: one fullscreen frozen-frame surface. Owns the
//! [`Editor`] (document + interaction state machine), the display texture,
//! and app-level concerns — global shortcuts, save/copy, toasts, the
//! double-Esc discard, prefs persistence, and the docs screenshot hook.
//! All pointer interaction logic lives in `editor`; all painting in `ui`.

use std::path::PathBuf;

use eframe::egui::{
    self, CentralPanel, Color32, ColorImage, Context, Key, KeyboardShortcut, Modifiers, Pos2, Rect,
    TextureHandle, TextureOptions, Vec2, ViewportCommand,
};
use image::RgbaImage;

use crate::annotate::{Annotation, Shape, Style, Tool, composite_pixelates, pixelate_rects};
use crate::document::Document;
use crate::editor::{Editor, EscapeOutcome};
use crate::ui::{TOOLS, canvas, text_overlay, toolbar};
use crate::{clipboard, export, platform_files, prefs};

/// Starter palette for first runs; at runtime the palette is a
/// most-recently-used stack persisted via `prefs`.
const PALETTE: [Color32; 6] = [
    Color32::from_rgb(0xe0, 0x2d, 0x2d), // red
    Color32::from_rgb(0xf2, 0xd0, 0x2e), // yellow
    Color32::from_rgb(0x2f, 0xb3, 0x44), // green
    Color32::from_rgb(0x2f, 0x52, 0xe0), // blue
    Color32::WHITE,
    Color32::BLACK,
];

/// App-level result/error shown in the toolbar's status line for a while.
struct Toast {
    message: String,
    until: f64,
    is_error: bool,
}

pub struct ScreencapApp {
    editor: Editor,
    texture: Option<TextureHandle>,
    /// Pixelate rects currently baked into `texture`; rebuilt when they
    /// drift from the annotation list (add, move, undo, redo).
    baked_pixelates: Vec<Rect>,
    toolbar: toolbar::Toolbar,
    toast: Option<Toast>,
    /// Canvas size measured last frame, for shortcut handlers that refit
    /// the view before this frame's layout is known.
    last_canvas_size: Vec2,
    out_dir: PathBuf,
    /// Docs/dev hook (`SCRANNOTATE_SHOT`): save a window screenshot here
    /// once the UI settles, then quit.
    shot_path: Option<PathBuf>,
    shot_frames: u32,
    /// Demo scene active (`SCRANNOTATE_DEMO`) — keep the user's prefs out
    /// of it entirely.
    demo: bool,
    /// Identity of the captured monitor, consumed by the first-frame
    /// placement hook ([`Self::place_window`]); `None` for --from-file
    /// and for Wayland captures (the portal never says which monitor).
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    capture_display: Option<crate::capture::DisplayInfo>,
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    placed: bool,
    /// macOS enters simple fullscreen after the first frame. Refit for a
    /// couple of frames while that asynchronous resize settles; otherwise
    /// the capture stays fitted to the small startup window.
    #[cfg(target_os = "macos")]
    placement_refit_frames: u8,
}

impl ScreencapApp {
    pub fn new(
        img: RgbaImage,
        out_dir: PathBuf,
        select_full: bool,
        demo_mode: Option<String>,
        capture_display: Option<crate::capture::DisplayInfo>,
    ) -> Self {
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        let _ = capture_display; // no placement hook on other targets
        let min_dim = img.width().min(img.height());
        // Default sizes scale with the screenshot so strokes stay legible on
        // HiDPI captures; deliberately-set (persisted) sizes win over that.
        let saved = if demo_mode.is_some() {
            prefs::Prefs::default()
        } else {
            prefs::load()
        };
        let width = (f64::from(min_dim) / 450.0).clamp(2.0, 8.0).round();
        let stroke_width = saved.width.unwrap_or(width as f32);
        let font_size = saved.font_size.unwrap_or(stroke_width * 8.0);
        let persist_style = saved.width.is_some() || saved.font_size.is_some();
        let mut palette = saved.palette.unwrap_or_else(|| PALETTE.to_vec());
        palette.truncate(6);
        // The head of the MRU list is the active color.
        let color = palette.first().copied().unwrap_or(PALETTE[0]);

        let mut doc = Document::new(img);
        if select_full {
            // Initial state, not an edit: set directly, outside history.
            doc.region = Some(doc.image_rect());
        }
        let style = Style {
            color,
            width: stroke_width,
            font_size,
        };
        let editor = Editor::new(doc, style, palette, persist_style);

        let mut app = Self {
            editor,
            texture: None,
            baked_pixelates: Vec::new(),
            toolbar: toolbar::Toolbar::new(saved.ui_scale.unwrap_or_default()),
            toast: None,
            last_canvas_size: Vec2::new(1400.0, 850.0),
            out_dir,
            shot_path: std::env::var_os("SCRANNOTATE_SHOT").map(PathBuf::from),
            shot_frames: 0,
            demo: demo_mode.is_some(),
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            capture_display,
            #[cfg(any(target_os = "linux", target_os = "macos", windows))]
            placed: false,
            #[cfg(target_os = "macos")]
            placement_refit_frames: 0,
        };
        if let Some(mode) = demo_mode {
            app.seed_demo(&mode);
        }
        app
    }

    /// Canned scenes for the README screenshots
    /// (`SCRANNOTATE_DEMO=annotate|multiselect|text|picker`). One shared
    /// document places every tool's output on the synthetic desktop; the
    /// modes differ only in what is selected, mid-edit, or popped up.
    fn seed_demo(&mut self, mode: &str) {
        let style = |color, width: f32, font_size: f32| Style {
            color,
            width,
            font_size,
        };
        let rect = |x0: f32, y0: f32, x1: f32, y1: f32| {
            Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
        };
        let [red, yellow, green, blue, ..] = PALETTE;
        self.editor.style = style(red, 5.0, 36.0);
        self.editor.doc.region = Some(rect(430.0, 70.0, 1560.0, 950.0));

        // One annotation per tool, tied to the synthetic desktop's furniture.
        let doc = &mut self.editor.doc;
        doc.push(Annotation::new(
            Shape::Marker {
                pos: Pos2::new(492.0, 232.0),
                number: 1,
                target: Some(Pos2::new(556.0, 292.0)),
            },
            style(blue, 4.0, 26.0),
        ));
        doc.push(Annotation::new(
            Shape::Highlight {
                rect: rect(532.0, 276.0, 1180.0, 322.0),
            },
            style(yellow, 5.0, 36.0),
        ));
        doc.push(Annotation::new(
            Shape::Ellipse {
                rect: rect(880.0, 330.0, 1060.0, 392.0),
            },
            style(green, 5.0, 36.0),
        ));
        doc.push(Annotation::new(
            Shape::Marker {
                pos: Pos2::new(492.0, 434.0),
                number: 2,
                target: None,
            },
            style(blue, 4.0, 26.0),
        ));
        doc.push(Annotation::new(
            Shape::Pixelate {
                rect: rect(532.0, 404.0, 1010.0, 464.0),
            },
            style(red, 5.0, 36.0),
        ));
        doc.push(Annotation::new(
            Shape::Line {
                a: Pos2::new(540.0, 514.0),
                b: Pos2::new(1170.0, 514.0),
            },
            style(blue, 5.0, 36.0),
        ));
        let text = doc.push(Annotation::new(
            Shape::Text {
                pos: Pos2::new(560.0, 596.0),
                text: "Ship this build!".to_owned(),
            },
            style(red, 5.0, 44.0),
        ));
        // A hand-drawn wavy underline beneath the text.
        let points = (0..=33u8)
            .map(|i| {
                let x = 565.0 + f32::from(i) * 10.0;
                Pos2::new(x, 668.0 + 6.0 * (x / 18.0).sin())
            })
            .collect();
        doc.push(Annotation::new(
            Shape::Pen { points },
            style(green, 4.0, 36.0),
        ));
        let arrow = doc.push(Annotation::new(
            Shape::Arrow {
                a: Pos2::new(880.0, 560.0),
                b: Pos2::new(1090.0, 760.0),
            },
            style(red, 5.0, 36.0),
        ));
        let boxed = doc.push(Annotation::new(
            Shape::Rect {
                rect: rect(1108.0, 740.0, 1372.0, 850.0),
            },
            style(red, 5.0, 36.0),
        ));
        doc.marker_next = 3;

        self.editor.tool = Tool::Select;
        match mode {
            // Group chrome and the "For 3 Selected" settings target.
            "multiselect" => self.editor.selected.extend([text, arrow, boxed]),
            // The inline text editor open on the text, caret and all.
            "text" => self.editor.open_text_editor(text),
            _ => {
                self.editor.selected.insert(boxed);
                if mode == "picker" {
                    self.editor.color_picker = Some(self.editor.style.color);
                }
            }
        }
    }

    fn set_toast(&mut self, ctx: &Context, message: impl Into<String>, is_error: bool) {
        self.toast = Some(Toast {
            message: message.into(),
            until: ctx.input(|i| i.time) + 5.0,
            is_error,
        });
        ctx.request_repaint_after(std::time::Duration::from_secs(5));
    }

    /// The region (or the whole frame when nothing is selected) with
    /// annotations rendered in.
    fn rendered(&self) -> anyhow::Result<RgbaImage> {
        let doc = &self.editor.doc;
        export::render_to_image(&doc.base, doc.shapes(), doc.region)
    }

    /// Save the region; optionally quit. On failure stay open either way.
    fn save(&mut self, ctx: &Context, close: bool) {
        if !self.prepare_export(ctx) {
            return;
        }
        let result = self
            .rendered()
            .and_then(|img| platform_files::save_image(&img, &self.out_dir));
        self.apply_save_result(ctx, close, result);
    }

    fn apply_save_result(
        &mut self,
        ctx: &Context,
        close: bool,
        result: anyhow::Result<Option<PathBuf>>,
    ) {
        match result {
            Ok(Some(path)) => {
                if let Some(directory) = path.parent() {
                    self.out_dir = directory.to_owned();
                }
                println!("{}", path.display());
                if close {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                } else {
                    self.set_toast(ctx, format!("Saved {}", path.display()), false);
                }
            }
            Ok(None) => self.set_toast(ctx, "Save cancelled.", false),
            Err(err) => self.set_toast(ctx, format!("Save failed: {err:#}"), true),
        }
    }

    #[cfg(any(windows, target_os = "macos", test))]
    fn needs_open_confirmation(&self) -> bool {
        self.editor.state.is_text_editing()
            || self.editor.state.is_pointer_op()
            || self.editor.doc.can_undo()
            || self.editor.doc.can_redo()
            || !self.editor.doc.annotations().is_empty()
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn open_image(&mut self, ctx: &Context) {
        if self.needs_open_confirmation()
            && rfd::MessageDialog::new()
                .set_title("Open another image?")
                .set_description("Opening another image will discard the current annotations and selection. Choose OK to select a PNG, or Cancel to keep editing. Your current image stays open if you cancel the file chooser.")
                .set_level(rfd::MessageLevel::Warning)
                .set_buttons(rfd::MessageButtons::OkCancel)
                .show() != rfd::MessageDialogResult::Ok
        {
            self.restore_text_focus(ctx);
            return;
        }
        self.apply_open_result(ctx, platform_files::open_image(None));
        self.restore_text_focus(ctx);
    }

    #[cfg(any(windows, target_os = "macos", test))]
    fn apply_open_result(&mut self, ctx: &Context, result: anyhow::Result<Option<RgbaImage>>) {
        match result {
            Ok(Some(image)) => {
                let mut doc = Document::new(image);
                doc.region = Some(doc.image_rect());
                self.editor = Editor::new(
                    doc,
                    self.editor.style,
                    self.editor.palette.clone(),
                    self.editor.persist_style,
                );
                self.editor
                    .view
                    .fit(self.editor.doc.image_size(), self.last_canvas_size);
                self.texture = None;
                self.baked_pixelates.clear();
                self.toolbar = toolbar::Toolbar::new(self.toolbar.ui_scale);
                self.toast = None;
                ctx.memory_mut(|memory| memory.stop_text_input());
                ctx.request_repaint();
            }
            Ok(None) => {}
            Err(error) => self.set_toast(ctx, format!("Open failed: {error:#}"), true),
        }
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn choose_output_directory(&mut self, ctx: &Context) {
        let result = platform_files::choose_output_directory(&self.out_dir);
        match result {
            Ok(Some(directory)) => {
                self.set_toast(ctx, format!("Saving to {}", directory.display()), false);
                self.out_dir = directory;
            }
            Ok(None) => {}
            Err(error) => self.set_toast(
                ctx,
                format!("Could not change save folder: {error:#}"),
                true,
            ),
        }
        self.restore_text_focus(ctx);
    }

    #[cfg(any(windows, target_os = "macos"))]
    fn restore_text_focus(&self, ctx: &Context) {
        if self.editor.state.is_text_editing() {
            // The inline editor uses this stable id to keep its caret state.
            ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("text-editor-input")));
        }
    }

    /// Copy the region to the clipboard; optionally quit. On failure stay
    /// open either way.
    fn copy(&mut self, ctx: &Context, close: bool) {
        if !self.prepare_export(ctx) {
            return;
        }
        match self.rendered().and_then(|img| clipboard::copy_image(&img)) {
            Ok(()) => {
                if close {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                } else {
                    self.set_toast(ctx, "Copied to the clipboard", false);
                }
            }
            Err(err) => self.set_toast(ctx, format!("Copy failed: {err:#}"), true),
        }
    }

    /// Export only completed interactions. In particular, an unfinished blur
    /// exists in the pointer state, not in the document being exported.
    fn prepare_export(&mut self, ctx: &Context) -> bool {
        if self.editor.state.is_pointer_op() {
            self.set_toast(
                ctx,
                "Finish or cancel the current drag before saving or copying.",
                true,
            );
            return false;
        }
        self.editor.commit_text();
        true
    }

    /// TextEdit must process this frame's text input before Save commits it.
    /// Its local editing shortcuts stay with the widget; application lifecycle
    /// commands still work while the caret is active.
    fn handle_text_lifecycle_shortcuts(&mut self, ctx: &Context) {
        match self.text_lifecycle_action(ctx) {
            Some(toolbar::ToolbarAction::Save { close }) => self.save(ctx, close),
            Some(toolbar::ToolbarAction::Close) => ctx.send_viewport_cmd(ViewportCommand::Close),
            _ => {}
        }
    }

    fn text_lifecycle_action(&self, ctx: &Context) -> Option<toolbar::ToolbarAction> {
        if ctx.memory(|m| m.top_modal_layer().is_some()) || !self.editor.state.is_text_editing() {
            return None;
        }
        let (save, quit) = ctx.input_mut(|i| {
            let command = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
            (
                i.consume_shortcut(&command(Key::S)),
                i.consume_shortcut(&command(Key::Q)) || i.consume_shortcut(&command(Key::W)),
            )
        });
        if save {
            Some(toolbar::ToolbarAction::Save { close: true })
        } else if quit {
            Some(toolbar::ToolbarAction::Close)
        } else {
            None
        }
    }

    /// Rebuild the display texture when the pixelate set changed.
    fn sync_texture(&mut self, ctx: &Context) {
        let doc = &self.editor.doc;
        let current = pixelate_rects(doc.shapes());
        if self.texture.is_some() && current == self.baked_pixelates {
            return;
        }
        let mut composited = doc.base.clone();
        composite_pixelates(&mut composited, doc.shapes());
        // GPUs cap texture sides (commonly 8192), and an all-monitors
        // capture can exceed that — egui panics on oversized uploads. The
        // *display* texture is downscaled to fit; export renders from the
        // full-resolution image, so output is unaffected.
        let max_side = u32::try_from(ctx.input(|i| i.max_texture_side))
            .unwrap_or(u32::MAX)
            .max(64);
        let (w, h) = composited.dimensions();
        if w > max_side || h > max_side {
            let long = u64::from(w.max(h));
            let scaled = |d: u32| {
                u32::try_from(u64::from(d) * u64::from(max_side) / long)
                    .unwrap_or(1)
                    .max(1)
            };
            composited = image::imageops::resize(
                &composited,
                scaled(w),
                scaled(h),
                image::imageops::FilterType::Triangle,
            );
        }
        let size = [composited.width() as usize, composited.height() as usize];
        let color_image = ColorImage::from_rgba_unmultiplied(size, composited.as_raw());
        match &mut self.texture {
            Some(tex) => tex.set(color_image, TextureOptions::LINEAR),
            None => {
                self.texture =
                    Some(ctx.load_texture("screenshot", color_image, TextureOptions::LINEAR));
            }
        }
        self.baked_pixelates = current;
    }

    fn handle_shortcuts(&mut self, ctx: &Context, canvas_size: Vec2) {
        if ctx.memory(|m| m.top_modal_layer().is_some()) {
            return;
        }
        // The inline text editor owns the keyboard completely.
        if self.editor.state.is_text_editing() {
            return;
        }
        // Plain keys must not fire while some widget (a slider, a drag
        // value…) holds keyboard focus — but modifier shortcuts stay live;
        // stray focus must never eat Ctrl+C.
        let focused = ctx.memory(|m| m.focused().is_some());
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let cmd_shift = |key| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key);
        let (save, copy, undo, redo, quit, fit, escape, reset, delete, select_all, space, tool) =
            ctx.input_mut(|i| {
                let save = i.consume_shortcut(&cmd(Key::S));
                // egui-winit swallows Ctrl+C before key handling and emits a
                // synthetic Copy event instead — a Ctrl+C KeyboardShortcut
                // can never fire, so watch the event stream. Gated on focus
                // so copying text out of a field (a slider's DragValue, the
                // color picker) can't overwrite the clipboard and quit;
                // widgets surrender focus after use, so the canvas normally
                // has it.
                let copy = !focused
                    && (i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                        || i.key_pressed(Key::Enter));
                let undo = i.consume_shortcut(&cmd(Key::Z));
                let redo =
                    i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y));
                let quit = i.consume_shortcut(&cmd(Key::Q)) || i.consume_shortcut(&cmd(Key::W));
                let fit =
                    (!focused && i.key_pressed(Key::F)) || i.consume_shortcut(&cmd(Key::Num0));
                let escape = !focused && i.key_pressed(Key::Escape) && !i.modifiers.shift;
                let reset = i.key_pressed(Key::Escape) && i.modifiers.shift;
                let delete =
                    !focused && (i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace));
                // Not consumed while a widget is focused — Ctrl+A must stay
                // select-all-text inside a focused field.
                let select_all = !focused && i.consume_shortcut(&cmd(Key::A));
                let space = !focused && i.key_released(Key::Space);
                let mut tool = None;
                if !focused && !i.modifiers.command {
                    for (t, key) in TOOLS {
                        if i.key_pressed(key) {
                            tool = Some(t);
                        }
                    }
                }
                (
                    save, copy, undo, redo, quit, fit, escape, reset, delete, select_all, space,
                    tool,
                )
            });
        if let Some(tool) = tool {
            self.editor.set_tool(tool);
        }
        if space {
            self.editor.space_released();
        }
        if save {
            self.save(ctx, true);
        }
        if copy {
            self.copy(ctx, true);
        }
        if redo {
            self.editor.redo();
        } else if undo {
            self.editor.undo();
        }
        if fit {
            self.editor
                .view
                .fit(self.editor.doc.image_size(), canvas_size);
        }
        if reset {
            self.editor.reset_all();
        }
        if select_all {
            self.editor.select_all();
        }
        if delete {
            self.editor.delete_selected();
        }
        if escape {
            let now = ctx.input(|i| i.time);
            match self.editor.escape(now) {
                EscapeOutcome::Consumed => {}
                // Second Esc of the discard confirmation: quit.
                EscapeOutcome::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
            }
        }
        if quit {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    /// First frame only: put the editor window on the captured monitor.
    ///
    /// - Windows and Linux/X11: `SetMonitor` re-fullscreens onto that exact
    ///   monitor (winit uses its physical bounds, so mixed-DPI setups stay
    ///   correct). Monitors match by name — `\\.\DISPLAYn` on Windows, the
    ///   RandR name (`DP-1`) on X11 — with size as the fallback.
    /// - macOS: position onto the monitor, then winit's "simple fullscreen"
    ///   — instant and in-place, where native fullscreen would animate onto
    ///   a new Space. `set_borderless_game` hard-hides the menu bar and
    ///   Dock instead of auto-revealing them at the screen edges.
    ///
    /// Wayland never gets a target: the portal doesn't say which monitor
    /// was picked, the compositor owns placement anyway, and
    /// `with_fullscreen(true)` at creation is all an app can ask for.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn place_window(&mut self, ctx: &Context, frame: &eframe::Frame) {
        if self.placed {
            return;
        }
        self.placed = true;
        if self.demo {
            return; // demo runs windowed for deterministic screenshots
        }
        let Some(win) = frame.winit_window() else {
            return;
        };
        let target = self.capture_display.take();

        #[cfg(any(windows, target_os = "linux"))]
        {
            let Some(target) = target else { return };
            let target_name = target.name.as_str();
            let monitors: Vec<_> = win.available_monitors().collect();
            if win
                .current_monitor()
                .is_some_and(|monitor| monitor.name().as_deref() == Some(target_name))
            {
                return;
            }
            // Prefer the device-name match; sizes disambiguate if winit and
            // the capture backend ever disagree on names.
            let index = monitors
                .iter()
                .position(|monitor| monitor.name().as_deref() == Some(target_name))
                .or_else(|| {
                    monitors.iter().position(|monitor| {
                        let size = monitor.size();
                        size.width == target.width && size.height == target.height
                    })
                });
            if let Some(index) = index {
                ctx.send_viewport_cmd(egui::ViewportCommand::SetMonitor(index));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
        }

        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{MonitorHandleExtMacOS, WindowExtMacOS};
            if let Some(target) = target
                && let Ok(display_id) = target.id.parse::<u32>()
                && let Some(monitor) = win
                    .available_monitors()
                    .find(|m| m.native_id() == display_id)
            {
                win.set_outer_position(monitor.position());
            }
            win.set_borderless_game(true);
            win.set_simple_fullscreen(true);
            self.placement_refit_frames = 2;
            ctx.request_repaint();
        }
    }

    /// Docs/dev hook: request a window screenshot once the UI settles,
    /// save it, quit.
    fn poll_shot_hook(&mut self, ctx: &Context) {
        if self.shot_path.is_none() {
            return;
        }
        self.shot_frames += 1;
        if self.shot_frames == 15 {
            ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
        }
        let shot = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = shot
            && let Some(path) = self.shot_path.take()
        {
            let (w, h) = (
                u32::try_from(image.size[0]).unwrap_or(0),
                u32::try_from(image.size[1]).unwrap_or(0),
            );
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            match RgbaImage::from_raw(w, h, bytes) {
                Some(shot) => {
                    if let Err(err) = shot.save(&path) {
                        eprintln!("screenshot save failed: {err}");
                    } else {
                        println!("{}", path.display());
                    }
                }
                None => eprintln!("screenshot buffer size mismatch"),
            }
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        ctx.request_repaint();
    }
}

impl eframe::App for ScreencapApp {
    fn on_exit(&mut self) {
        if self.demo {
            return;
        }
        // The invariant (head == active color) makes the palette the whole
        // color state worth keeping.
        let color = self.editor.style.color;
        self.editor.promote_color(color);
        prefs::save(
            &self.editor.palette,
            self.editor.persist_style.then_some(&self.editor.style),
            self.toolbar.ui_scale,
        );
    }

    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let ctx = &ctx;
        #[cfg(any(target_os = "linux", target_os = "macos", windows))]
        self.place_window(ctx, _frame);
        self.sync_texture(ctx);
        self.poll_shot_hook(ctx);

        // Time-driven state (the discard confirmation) expires in the FSM;
        // keep repainting while it's armed so the expiry actually shows.
        let now = ctx.input(|i| i.time);
        self.editor.tick(now);
        if self.editor.state.is_confirm_discard() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }

        self.handle_shortcuts(ctx, self.last_canvas_size);

        CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::BLACK))
            .show(root, |ui| {
                let canvas = canvas::show(ui, &mut self.editor, self.texture.as_ref());
                self.last_canvas_size = canvas.size();

                // App-level results/errors override the editor's status
                // line for as long as they're fresh.
                if self.toast.as_ref().is_some_and(|t| t.until <= now) {
                    self.toast = None;
                }
                let status_override = self.toast.as_ref().map(|t| toolbar::StatusOverride {
                    message: t.message.clone(),
                    is_error: t.is_error,
                });

                match self
                    .toolbar
                    .show(ctx, &mut self.editor, canvas, status_override)
                {
                    // The click landed on the toolbar, not the canvas, so
                    // an open text edit hasn't committed yet — what's
                    // exported must be what's on screen.
                    Some(toolbar::ToolbarAction::Copy { close }) => {
                        self.editor.commit_text();
                        self.copy(ctx, close);
                    }
                    Some(toolbar::ToolbarAction::Save { close }) => {
                        self.editor.commit_text();
                        self.save(ctx, close);
                    }
                    Some(toolbar::ToolbarAction::Close) => {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                    #[cfg(any(windows, target_os = "macos"))]
                    Some(toolbar::ToolbarAction::OpenImage) => self.open_image(ctx),
                    #[cfg(any(windows, target_os = "macos"))]
                    Some(toolbar::ToolbarAction::ChooseOutputDirectory) => {
                        self.choose_output_directory(ctx)
                    }
                    None => {}
                }
                // Ctrl+C with nothing selected and the caret at the end is
                // not a text operation — it means the same thing it does
                // outside the editor, with the text on screen included.
                if !ctx.memory(|m| m.top_modal_layer().is_some())
                    && let Some(text_overlay::TextEditAction::CopyAndClose) =
                        text_overlay::show(ctx, &mut self.editor, canvas)
                {
                    self.editor.commit_text();
                    self.copy(ctx, true);
                }
                self.handle_text_lifecycle_shortcuts(ctx);
            });

        // set_simple_fullscreen takes effect after the first layout. Make
        // the next layouts fit the capture to the resized fullscreen canvas.
        #[cfg(target_os = "macos")]
        if self.placement_refit_frames > 0 {
            self.placement_refit_frames -= 1;
            self.editor.view.fitted = false;
            ctx.request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::state::{EditorState, TextEditState};

    fn app() -> ScreencapApp {
        ScreencapApp::new(RgbaImage::new(200, 120), PathBuf::new(), true, None, None)
    }

    fn edit_text(app: &mut ScreencapApp) {
        app.editor.state = EditorState::TextEditing(TextEditState {
            target: None,
            pos: Pos2::new(10.0, 10.0),
            buffer: "Keep this text".into(),
            style: app.editor.style,
            just_created: false,
            last_rect: None,
        });
    }

    fn command_input(key: Key) -> egui::RawInput {
        let modifiers = Modifiers {
            command: true,
            ctrl: true,
            ..Modifiers::NONE
        };
        egui::RawInput {
            events: vec![
                egui::Event::ModifiersChanged(modifiers),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn export_rejects_unfinished_blur_without_closing_or_committing() {
        let mut app = app();
        app.editor.tool = Tool::Pixelate;
        app.editor.state = EditorState::DrawingShape {
            start: Pos2::ZERO,
            current: Pos2::new(50.0, 50.0),
            points: Vec::new(),
        };
        let ctx = Context::default();
        let output =
            crate::test_support::run_ui(&ctx, Default::default(), |_| app.save(&ctx, true));
        assert!(app.editor.state.is_pointer_op());
        assert_eq!(app.editor.doc.shapes().count(), 0);
        assert!(app.toast.as_ref().is_some_and(|toast| toast.is_error));
        assert!(!output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::Close))
        }));
    }

    #[test]
    fn save_shortcut_commits_text_and_keeps_editor_open_on_failure() {
        let mut app = app();
        edit_text(&mut app);
        // The test executable is a file, so it cannot be created as an output
        // directory. No writes occur; failure must preserve the text.
        app.out_dir = std::env::current_exe().expect("test executable");
        let ctx = Context::default();
        let output = crate::test_support::run_ui(&ctx, command_input(Key::S), |_| {
            let Some(toolbar::ToolbarAction::Save { close }) = app.text_lifecycle_action(&ctx)
            else {
                panic!("Save shortcut must remain available while typing");
            };
            assert!(app.prepare_export(&ctx));
            // Exercise the same write failure without invoking a native
            // authorization panel in mac-app-store CI builds.
            let result = app
                .rendered()
                .and_then(|image| export::save_timestamped(&image, &app.out_dir))
                .map(Some);
            app.apply_save_result(&ctx, close, result);
        });
        assert!(!app.editor.state.is_text_editing());
        assert!(app.editor.doc.shapes().any(|annotation| {
            matches!(&annotation.shape, Shape::Text { text, .. } if text == "Keep this text")
        }));
        assert!(app.toast.as_ref().is_some_and(|toast| toast.is_error));
        assert!(!output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::Close))
        }));
    }

    #[test]
    fn cancelled_save_keeps_the_editor_open() {
        let mut app = app();
        edit_text(&mut app);
        let original = app.editor.doc.base.clone();
        let ctx = Context::default();
        let output = crate::test_support::run_ui(&ctx, Default::default(), |_| {
            assert!(app.prepare_export(&ctx));
            app.apply_save_result(&ctx, true, Ok(None));
        });
        assert_eq!(app.editor.doc.base, original);
        assert!(app.editor.doc.shapes().any(|annotation| {
            matches!(&annotation.shape, Shape::Text { text, .. } if text == "Keep this text")
        }));
        assert!(
            app.toast
                .as_ref()
                .is_some_and(|toast| !toast.is_error && toast.message == "Save cancelled.")
        );
        assert!(!output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::Close))
        }));
    }

    #[test]
    fn cancelled_or_failed_open_preserves_the_current_document_and_text() {
        let mut app = app();
        app.editor
            .doc
            .base
            .put_pixel(0, 0, image::Rgba([5, 10, 15, 255]));
        app.editor.view.pan_by(Vec2::new(17.0, 23.0));
        edit_text(&mut app);
        let original = app.editor.doc.base.clone();
        let original_region = app.editor.doc.region;
        let original_style = app.editor.style;
        let ctx = Context::default();
        for result in [Ok(None), Err(anyhow::anyhow!("damaged PNG"))] {
            app.apply_open_result(&ctx, result);
            assert_eq!(app.editor.doc.base, original);
            assert_eq!(app.editor.doc.region, original_region);
            assert_eq!(app.editor.style, original_style);
            assert_eq!(app.editor.view.pan, Vec2::new(17.0, 23.0));
            let EditorState::TextEditing(edit) = &app.editor.state else {
                panic!("Opening did not succeed, so the in-progress text must remain");
            };
            assert_eq!(edit.buffer, "Keep this text");
        }
        assert!(app.toast.as_ref().is_some_and(|toast| toast.is_error));
    }

    #[test]
    fn opening_requires_confirmation_for_edits_and_active_interactions() {
        let mut app = app();
        assert!(!app.needs_open_confirmation());
        edit_text(&mut app);
        assert!(app.needs_open_confirmation());
        app.editor.state = EditorState::DrawingShape {
            start: Pos2::ZERO,
            current: Pos2::new(40.0, 40.0),
            points: vec![],
        };
        assert!(app.needs_open_confirmation());
        app.editor.state = EditorState::Idle;
        app.editor.doc.begin();
        app.editor.doc.region = Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0)));
        app.editor.doc.commit();
        assert!(
            app.needs_open_confirmation(),
            "changed capture region must be confirmed"
        );
    }

    #[test]
    fn successful_open_resets_document_and_texture_but_keeps_style_preferences() {
        let mut app = app();
        app.editor.style = Style {
            color: Color32::BLUE,
            width: 9.0,
            font_size: 44.0,
        };
        app.editor.palette = vec![Color32::BLUE, Color32::GREEN];
        app.editor.persist_style = true;
        let style = app.editor.style;
        let palette = app.editor.palette.clone();
        app.editor.doc.begin();
        let annotation = app.editor.doc.push(Annotation::new(
            Shape::Pixelate {
                rect: Rect::from_min_size(Pos2::ZERO, Vec2::splat(15.0)),
            },
            style,
        ));
        app.editor.doc.commit();
        app.editor.selected.insert(annotation);
        app.editor.view.pan_by(Vec2::new(100.0, 50.0));
        edit_text(&mut app);
        let ctx = Context::default();
        let _ = crate::test_support::run_ui(&ctx, Default::default(), |_| app.sync_texture(&ctx));
        assert!(app.texture.is_some());
        assert!(!app.baked_pixelates.is_empty());
        let new_image = RgbaImage::from_pixel(30, 20, image::Rgba([40, 80, 120, 255]));
        app.apply_open_result(&ctx, Ok(Some(new_image.clone())));
        assert_eq!(app.editor.doc.base, new_image);
        assert_eq!(app.editor.doc.region, Some(app.editor.doc.image_rect()));
        assert!(app.editor.doc.annotations().is_empty());
        assert!(!app.editor.doc.can_undo());
        assert!(!app.editor.doc.can_redo());
        assert!(app.editor.selected.is_empty());
        assert!(app.editor.state.is_idle());
        assert_eq!(app.editor.tool, Tool::Select);
        assert!(app.editor.view.fitted);
        assert_eq!(app.editor.style, style);
        assert_eq!(app.editor.palette, palette);
        assert!(app.editor.persist_style);
        assert!(app.texture.is_none());
        assert!(app.baked_pixelates.is_empty());
    }

    #[test]
    fn quit_shortcut_works_during_text_editing() {
        let mut app = app();
        edit_text(&mut app);
        let ctx = Context::default();
        let output = crate::test_support::run_ui(&ctx, command_input(Key::Q), |_| {
            app.handle_text_lifecycle_shortcuts(&ctx);
        });
        assert!(output.viewport_output.values().any(|viewport| {
            viewport
                .commands
                .iter()
                .any(|command| matches!(command, ViewportCommand::Close))
        }));
    }
}
