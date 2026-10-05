//! Rasterize annotations onto the screenshot for saving/copying. Geometry
//! comes from `annotate` so this stays in lockstep with the egui renderer;
//! text uses egui's own layout, fallback fonts, and glyph atlas.

use anyhow::{Context, Result};
use eframe::egui::{Color32, Pos2, epaint::text::Fonts};
use image::RgbaImage;
use tiny_skia::{FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};

use crate::annotate::{
    Annotation, Shape, arrow_geometry, clamp_px, composite_pixelates, highlight_color,
    marker_radius,
};

mod text;

/// Save `img` into `dir` under the timestamped scrannotate name, creating
/// the directory if needed. Returns the written path.
pub fn save_timestamped(img: &RgbaImage, dir: &std::path::Path) -> Result<std::path::PathBuf> {
    let stem = format!(
        "scrannotate-{}",
        chrono::Local::now().format("%Y-%m-%d_%H%M%S")
    );
    save_png_named(img, dir, &stem)
}

fn save_png_named(
    img: &RgbaImage,
    dir: &std::path::Path,
    stem: &str,
) -> Result<std::path::PathBuf> {
    use std::io::Write as _;

    save_unique(dir, stem, |file| {
        let mut writer = std::io::BufWriter::new(file);
        img.write_to(&mut writer, image::ImageFormat::Png)?;
        writer.flush()?;
        Ok(())
    })
}

/// `create_new` reserves each name atomically, including across app instances.
/// Keep the usual name for the first save and add -1, -2, … on collisions.
fn save_unique(
    dir: &std::path::Path,
    stem: &str,
    write: impl FnOnce(&mut std::fs::File) -> Result<()>,
) -> Result<std::path::PathBuf> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for suffix in 0_u64.. {
        let name = if suffix == 0 {
            format!("{stem}.png")
        } else {
            format!("{stem}-{suffix}.png")
        };
        let path = dir.join(name);
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err).with_context(|| format!("creating {}", path.display())),
        };
        let result = write(&mut file).and_then(|()| Ok(file.sync_all()?));
        // Windows cannot remove the incomplete file until its handle is closed.
        drop(file);
        if let Err(err) = result {
            if let Err(cleanup) = std::fs::remove_file(&path) {
                return Err(err).with_context(|| {
                    format!(
                        "saving {}; could not remove incomplete file: {cleanup}",
                        path.display()
                    )
                });
            }
            return Err(err).with_context(|| format!("saving {}", path.display()));
        }
        return Ok(path);
    }
    anyhow::bail!("no unused screenshot filename in {}", dir.display())
}

pub fn render_to_image<'a>(
    base: &RgbaImage,
    annotations: impl IntoIterator<Item = &'a Annotation> + Clone,
    crop: Option<eframe::egui::Rect>,
) -> Result<RgbaImage> {
    let mut img = base.clone();
    composite_pixelates(&mut img, annotations.clone());

    let (w, h) = img.dimensions();
    // tiny-skia uses premultiplied RGBA, while PNG/image/clipboard use
    // straight RGBA. Render an overlay, then composite it into the original
    // buffer. This also preserves all untouched pixels exactly, including
    // hidden RGB at alpha zero and low-alpha colors lost by an 8-bit roundtrip.
    let mut pixmap = Pixmap::new(w, h).context("empty image or image too large")?;
    let mut fonts = Fonts::new(Default::default(), Default::default());
    for ann in annotations {
        draw_annotation(&mut pixmap, &mut fonts, ann)?;
    }
    composite_overlay(&mut img, &pixmap);
    let full = img;

    let Some(crop) = crop else { return Ok(full) };
    let x0 = clamp_px(crop.min.x.round(), w.saturating_sub(1));
    let y0 = clamp_px(crop.min.y.round(), h.saturating_sub(1));
    let cw = clamp_px(crop.width().round(), w - x0).max(1);
    let ch = clamp_px(crop.height().round(), h - y0).max(1);
    Ok(image::imageops::crop_imm(&full, x0, y0, cw, ch).to_image())
}

/// Grow `img` by a solid border of `width` px on every side — the outset
/// form: nothing captured is painted over, the output simply gets larger.
///
/// An export-stage step rather than an annotation on purpose. Annotations
/// are drawn onto the full image and only cropped to the region at the very
/// end, so a border drawn as one would be cropped straight off whenever a
/// region is selected; and growing the canvas is not something a shape
/// positioned in image coordinates can express.
pub fn with_border(img: &RgbaImage, width: u32, color: Color32) -> RgbaImage {
    if width == 0 {
        return img.clone();
    }
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let (w, h) = img.dimensions();
    let mut out = RgbaImage::from_pixel(w + width * 2, h + width * 2, image::Rgba([r, g, b, a]));
    image::imageops::replace(&mut out, img, i64::from(width), i64::from(width));
    out
}

/// Border thickness for an exported image of `size`, in px. Scaled with the
/// image rather than fixed, so it reads the same on a 4K capture as on a
/// 1080p one (a fixed 8px all but vanishes on the former), and clamped so
/// neither extreme is absurd: never a hairline, never a picture mount.
///
/// Roughly 1/70 of the shorter side — about 15px on a 1080p-tall export.
/// A thinner rule (1/135, ~7px there) was legible against the content but
/// too timid at the job people actually want this for: separating a
/// screenshot from the white page it gets pasted onto.
pub fn border_width(size: (u32, u32)) -> u32 {
    let min_dim = f64::from(size.0.min(size.1));
    (min_dim / 70.0).clamp(8.0, 32.0).round() as u32
}

/// Source-over from a premultiplied overlay into a straight-alpha image.
fn composite_overlay(base: &mut RgbaImage, overlay: &Pixmap) {
    for (dst, src) in base.pixels_mut().zip(overlay.pixels()) {
        if src.alpha() == 0 {
            continue;
        }
        let remaining = 1.0 - f32::from(src.alpha()) / 255.0;
        let dst_alpha = f32::from(dst[3]) / 255.0;
        let alpha = f32::from(src.alpha()) / 255.0 + dst_alpha * remaining;
        for (channel, premultiplied) in [src.red(), src.green(), src.blue()].into_iter().enumerate()
        {
            dst[channel] = ((f32::from(premultiplied)
                + f32::from(dst[channel]) * dst_alpha * remaining)
                / alpha)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        dst[3] = (alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    }
}

fn solid_paint(color: Color32) -> Paint<'static> {
    let [r, g, b, a] = color.to_srgba_unmultiplied();
    let mut paint = Paint::default();
    paint.set_color_rgba8(r, g, b, a);
    paint.anti_alias = true;
    paint
}

fn round_stroke(width: f32) -> Stroke {
    Stroke {
        width,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    }
}

fn skia_rect(rect: eframe::egui::Rect) -> Option<tiny_skia::Rect> {
    tiny_skia::Rect::from_ltrb(rect.min.x, rect.min.y, rect.max.x, rect.max.y)
}

/// Rigid rotation around a shape's center (stroke widths are unaffected).
fn rotation_transform(rotation: f32, center: Pos2) -> Transform {
    if rotation == 0.0 {
        Transform::identity()
    } else {
        Transform::from_rotate_at(rotation.to_degrees(), center.x, center.y)
    }
}

fn draw_annotation(pixmap: &mut Pixmap, fonts: &mut Fonts, ann: &Annotation) -> Result<()> {
    let style = &ann.style;
    let paint = solid_paint(style.color);
    let stroke = round_stroke(style.width);
    let identity = Transform::identity();

    match &ann.shape {
        Shape::Pen { points } => {
            if points.len() < 2 {
                if let Some(p) = points.first()
                    && let Some(dot) = PathBuilder::from_circle(p.x, p.y, style.width * 0.5)
                {
                    pixmap.fill_path(&dot, &paint, FillRule::Winding, identity, None);
                }
                return Ok(());
            }
            let mut pb = PathBuilder::new();
            pb.move_to(points[0].x, points[0].y);
            for p in &points[1..] {
                pb.line_to(p.x, p.y);
            }
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, identity, None);
            }
        }
        Shape::Line { a, b } => {
            let mut pb = PathBuilder::new();
            pb.move_to(a.x, a.y);
            pb.line_to(b.x, b.y);
            if let Some(path) = pb.finish() {
                pixmap.stroke_path(&path, &paint, &stroke, identity, None);
            }
        }
        Shape::Arrow { a, b } => {
            draw_arrow(pixmap, &paint, &stroke, *a, *b, style.width);
        }
        Shape::Rect { rect } => {
            if let Some(r) = skia_rect(*rect) {
                let path = PathBuilder::from_rect(r);
                let transform = rotation_transform(ann.rotation, rect.center());
                pixmap.stroke_path(&path, &paint, &stroke, transform, None);
            }
        }
        Shape::Ellipse { rect } => {
            if let Some(r) = skia_rect(*rect)
                && let Some(path) = PathBuilder::from_oval(r)
            {
                let transform = rotation_transform(ann.rotation, rect.center());
                pixmap.stroke_path(&path, &paint, &stroke, transform, None);
            }
        }
        Shape::Highlight { rect } => {
            if let Some(r) = skia_rect(*rect) {
                let path = PathBuilder::from_rect(r);
                let fill = solid_paint(highlight_color(style.color));
                let transform = rotation_transform(ann.rotation, rect.center());
                pixmap.fill_path(&path, &fill, FillRule::Winding, transform, None);
            }
        }
        // Pixelation is baked into the base image before vector drawing.
        Shape::Pixelate { .. } => {}
        Shape::Text { pos, text } => {
            text::draw(pixmap, fonts, *pos, text, style, ann.rotation, false)?;
        }
        Shape::Marker {
            pos,
            number,
            target,
        } => {
            // Arrow first; the circle covers the shaft's root.
            if let Some(target) = target {
                draw_arrow(pixmap, &paint, &stroke, *pos, *target, style.width);
            }
            let radius = marker_radius(style);
            if let Some(circle) = PathBuilder::from_circle(pos.x, pos.y, radius) {
                pixmap.fill_path(&circle, &paint, FillRule::Winding, identity, None);
            }
            text::draw(
                pixmap,
                fonts,
                *pos,
                &number.to_string(),
                &crate::annotate::Style {
                    color: Color32::WHITE,
                    ..*style
                },
                0.0,
                true,
            )?;
        }
    }
    Ok(())
}

/// Shaft + filled head from `a` to `b` (shared by arrows and markers).
fn draw_arrow(
    pixmap: &mut Pixmap,
    paint: &Paint<'_>,
    stroke: &Stroke,
    a: Pos2,
    b: Pos2,
    width: f32,
) {
    let geo = arrow_geometry(a, b, width);
    let identity = Transform::identity();
    let mut pb = PathBuilder::new();
    pb.move_to(a.x, a.y);
    pb.line_to(geo.shaft_end.x, geo.shaft_end.y);
    if let Some(path) = pb.finish() {
        pixmap.stroke_path(&path, paint, stroke, identity, None);
    }
    let mut pb = PathBuilder::new();
    pb.move_to(geo.head[0].x, geo.head[0].y);
    pb.line_to(geo.head[1].x, geo.head[1].y);
    pb.line_to(geo.head[2].x, geo.head[2].y);
    pb.close();
    if let Some(path) = pb.finish() {
        pixmap.fill_path(&path, paint, FillRule::Winding, identity, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotate::Style;
    use eframe::egui::{Pos2, Rect};

    #[test]
    fn a_border_grows_the_image_and_paints_only_the_new_margin() {
        let base = RgbaImage::from_pixel(40, 30, image::Rgba([10, 20, 30, 255]));
        let white = Color32::WHITE;
        let out = with_border(&base, 5, white);
        assert_eq!(out.dimensions(), (50, 40), "grows by 2x width on each axis");
        // Every original pixel survives, shifted by the border width: the
        // outset form must never paint over what was captured.
        for y in 0..30 {
            for x in 0..40 {
                assert_eq!(
                    out.get_pixel(x + 5, y + 5),
                    base.get_pixel(x, y),
                    "({x},{y})"
                );
            }
        }
        // The new margin is the border color, corners included.
        for (x, y) in [(0, 0), (49, 0), (0, 39), (49, 39), (25, 2), (2, 20)] {
            assert_eq!(
                out.get_pixel(x, y),
                &image::Rgba([255, 255, 255, 255]),
                "({x},{y})"
            );
        }
    }

    #[test]
    fn a_zero_width_border_is_the_image_itself() {
        let base = RgbaImage::from_pixel(8, 6, image::Rgba([1, 2, 3, 4]));
        assert_eq!(with_border(&base, 0, Color32::RED), base);
    }

    #[test]
    fn border_width_scales_with_the_image_and_stays_within_bounds() {
        // Proportional in the middle of the range...
        assert_eq!(border_width((1920, 1080)), 15);
        assert_eq!(border_width((1380, 900)), 13);
        // ...and clamped at both ends, so a thumbnail gets no hairline and
        // a huge capture gets no picture mount.
        assert_eq!(border_width((100, 60)), 8);
        assert_eq!(border_width((8000, 6000)), 32);
        // Driven by the shorter side, whichever way round the image is.
        assert_eq!(border_width((4000, 700)), border_width((700, 4000)));
    }

    #[test]
    fn the_border_frames_the_cropped_region_not_the_whole_capture() {
        // The ordering that matters: annotations are drawn onto the full
        // image and cropped to the region at the very end, so a border
        // applied before the crop would be cropped straight off. Applied
        // after, it frames exactly what gets exported.
        let base = RgbaImage::from_pixel(400, 300, image::Rgba([10, 20, 30, 255]));
        let region = Rect::from_min_max(Pos2::new(100.0, 50.0), Pos2::new(300.0, 200.0));
        let cropped = render_to_image(&base, &[], Some(region)).unwrap();
        assert_eq!(cropped.dimensions(), (200, 150));
        let w = border_width(cropped.dimensions());
        let framed = with_border(&cropped, w, Color32::WHITE);
        assert_eq!(framed.dimensions(), (200 + w * 2, 150 + w * 2));
        // A border on the full capture would have been 400x300-sized and
        // lost to the crop; this one survives at the region's own edge.
        assert_eq!(framed.get_pixel(0, 0), &image::Rgba([255, 255, 255, 255]));
        assert_eq!(framed.get_pixel(w, w), cropped.get_pixel(0, 0));
    }

    fn style(color: Color32) -> Style {
        Style {
            color,
            width: 4.0,
            font_size: 24.0,
        }
    }

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "scrannotate-export-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("test directory");
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn repeated_saves_preserve_existing_files_and_use_suffixes() {
        let dir = TestDir::new();
        let stem = "scrannotate-2026-09-20_120000";
        let first = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let second = RgbaImage::from_pixel(2, 2, image::Rgba([0, 255, 0, 255]));
        let a = save_png_named(&first, &dir.0, stem).expect("first save");
        let b = save_png_named(&second, &dir.0, stem).expect("second save");
        assert_eq!(a.file_name().unwrap(), format!("{stem}.png").as_str());
        assert_eq!(b.file_name().unwrap(), format!("{stem}-1.png").as_str());
        assert_eq!(image::open(a).unwrap().into_rgba8(), first);
        assert_eq!(image::open(b).unwrap().into_rgba8(), second);
    }

    #[test]
    fn simultaneous_saves_reserve_distinct_names() {
        let dir = TestDir::new();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            let saves: Vec<_> = (0..8)
                .map(|i| {
                    let dir = &dir.0;
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let img = RgbaImage::from_pixel(2, 2, image::Rgba([i, 100, 200, 255]));
                        barrier.wait();
                        let path = save_png_named(&img, dir, "scrannotate-2026-09-20_120000")
                            .expect("concurrent save");
                        (path, img)
                    })
                })
                .collect();
            let mut paths = std::collections::HashSet::new();
            for save in saves {
                let (path, img) = save.join().expect("save thread");
                assert_eq!(image::open(&path).unwrap().into_rgba8(), img);
                assert!(paths.insert(path), "two saves returned the same path");
            }
        });
    }

    #[test]
    fn failed_save_removes_only_its_incomplete_file() {
        use std::io::Write as _;
        let dir = TestDir::new();
        let original = dir.0.join("scrannotate-fixed.png");
        std::fs::write(&original, b"existing image").unwrap();
        let result = save_unique(&dir.0, "scrannotate-fixed", |file| {
            file.write_all(b"partial PNG")?;
            Err(std::io::Error::other("injected write failure").into())
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(original).unwrap(), b"existing image");
        assert!(!dir.0.join("scrannotate-fixed-1.png").exists());
        assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn unchanged_pixels_preserve_straight_rgba_at_every_alpha() {
        let base = RgbaImage::from_fn(256, 32, |x, _| image::Rgba([37, 173, 241, x as u8]));
        assert_eq!(render_to_image(&base, &[], None).unwrap(), base);
        let ann = Annotation::new(
            Shape::Highlight {
                rect: Rect::from_min_max(Pos2::new(0.0, 24.0), Pos2::new(256.0, 32.0)),
            },
            style(Color32::RED),
        );
        let out = render_to_image(&base, &[ann], None).unwrap();
        for x in 0..256 {
            assert_eq!(out.get_pixel(x, 0), base.get_pixel(x, 0));
        }
    }

    #[test]
    fn transparent_highlights_export_straight_colors_and_roundtrip_png() {
        let dir = TestDir::new();
        let base = RgbaImage::new(32, 32);
        let ann = Annotation::new(
            Shape::Highlight {
                rect: Rect::from_min_max(Pos2::new(4.0, 4.0), Pos2::new(28.0, 28.0)),
            },
            style(Color32::RED),
        );
        let out = render_to_image(&base, std::slice::from_ref(&ann), None).unwrap();
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([255, 0, 0, 70]));
        let path = save_png_named(&out, &dir.0, "alpha").unwrap();
        assert_eq!(image::open(path).unwrap().into_rgba8(), out);

        let translucent = RgbaImage::from_pixel(32, 32, image::Rgba([0, 0, 255, 128]));
        let out = render_to_image(&translucent, &[ann], None).unwrap();
        assert_eq!(*out.get_pixel(10, 10), image::Rgba([110, 0, 145, 163]));
    }

    #[test]
    fn text_and_rotated_text_keep_straight_alpha() {
        let base = RgbaImage::new(128, 128);
        for rotation in [0.0, 0.4] {
            let ann = Annotation {
                shape: Shape::Text {
                    pos: Pos2::new(24.0, 32.0),
                    text: "A 🚀\nB".into(),
                },
                style: style(Color32::RED),
                rotation,
            };
            let out = render_to_image(&base, &[ann], None).unwrap();
            let painted: Vec<_> = out.pixels().filter(|p| p[3] != 0).collect();
            assert!(painted.len() > 100);
            assert!(
                painted.iter().any(|p| p[3] < 255),
                "antialiased edge pixels"
            );
            assert!(
                painted
                    .iter()
                    .all(|p| p[0] == 255 && p[1] == 0 && p[2] == 0)
            );
        }
    }

    #[test]
    fn distinct_fallback_emoji_export_distinct_glyphs() {
        let base = RgbaImage::new(96, 96);
        for rotation in [0.0, -0.3] {
            let render = |text: &str| {
                let ann = Annotation {
                    shape: Shape::Text {
                        pos: Pos2::new(24.0, 24.0),
                        text: text.into(),
                    },
                    style: Style {
                        font_size: 36.0,
                        ..style(Color32::WHITE)
                    },
                    rotation,
                };
                render_to_image(&base, &[ann], None).unwrap()
            };
            let face = render("😀");
            let rocket = render("🚀");
            assert_ne!(
                face, rocket,
                "fallback emoji became the same missing-glyph box"
            );
            assert!(face.pixels().any(|p| p[3] != 0));
            assert!(rocket.pixels().any(|p| p[3] != 0));
        }
    }

    #[test]
    fn render_all_annotation_kinds() {
        let base = RgbaImage::from_fn(900, 600, |x, y| {
            let checker = if (x / 40 + y / 40) % 2 == 0 { 40 } else { 70 };
            image::Rgba([
                u8::try_from(x * 255 / 900).unwrap_or(255),
                checker,
                u8::try_from(y * 255 / 600).unwrap_or(255),
                255,
            ])
        });
        let red = Color32::from_rgb(0xe0, 0x2d, 0x2d);
        let yellow = Color32::from_rgb(0xf2, 0xd0, 0x2e);
        let blue = Color32::from_rgb(0x2f, 0x52, 0xe0);
        let annotations = vec![
            Annotation::new(
                Shape::Pen {
                    points: (0..60)
                        .map(|i| {
                            let t = i as f32 / 59.0;
                            Pos2::new(40.0 + t * 200.0, 60.0 + (t * 12.0).sin() * 25.0)
                        })
                        .collect(),
                },
                style(red),
            ),
            Annotation::new(
                Shape::Line {
                    a: Pos2::new(40.0, 140.0),
                    b: Pos2::new(240.0, 180.0),
                },
                style(blue),
            ),
            Annotation::new(
                Shape::Arrow {
                    a: Pos2::new(40.0, 220.0),
                    b: Pos2::new(240.0, 300.0),
                },
                style(red),
            ),
            Annotation::new(
                Shape::Rect {
                    rect: Rect::from_min_max(Pos2::new(300.0, 60.0), Pos2::new(480.0, 180.0)),
                },
                style(red),
            ),
            Annotation::new(
                Shape::Ellipse {
                    rect: Rect::from_min_max(Pos2::new(300.0, 220.0), Pos2::new(480.0, 330.0)),
                },
                style(blue),
            ),
            Annotation::new(
                Shape::Highlight {
                    rect: Rect::from_min_max(Pos2::new(520.0, 60.0), Pos2::new(860.0, 120.0)),
                },
                style(yellow),
            ),
            Annotation::new(
                Shape::Pixelate {
                    rect: Rect::from_min_max(Pos2::new(520.0, 160.0), Pos2::new(860.0, 300.0)),
                },
                style(red),
            ),
            Annotation::new(
                Shape::Text {
                    pos: Pos2::new(40.0, 380.0),
                    text: "Annotated with scrannotate\nsecond line".to_owned(),
                },
                style(Color32::WHITE),
            ),
            Annotation::new(
                Shape::Marker {
                    pos: Pos2::new(600.0, 420.0),
                    number: 1,
                    target: None,
                },
                style(red),
            ),
            Annotation::new(
                Shape::Marker {
                    pos: Pos2::new(660.0, 420.0),
                    number: 12,
                    target: Some(Pos2::new(760.0, 520.0)),
                },
                style(blue),
            ),
            Annotation {
                shape: Shape::Rect {
                    rect: Rect::from_min_max(Pos2::new(560.0, 380.0), Pos2::new(700.0, 460.0)),
                },
                style: style(yellow),
                rotation: 0.5,
            },
            Annotation {
                shape: Shape::Text {
                    pos: Pos2::new(80.0, 480.0),
                    text: "rotated text".to_owned(),
                },
                style: style(red),
                rotation: -0.4,
            },
        ];

        let out = render_to_image(&base, &annotations, None).expect("render");
        assert_eq!(out.dimensions(), (900, 600));

        // Inside the pixelate region every pixel of an 8×8 mosaic block is
        // identical, which is never true of the gradient base.
        let block: Vec<_> = (552..560)
            .flat_map(|x| (160..168).map(move |y| (x, y)))
            .map(|(x, y)| *out.get_pixel(x, y))
            .collect();
        assert!(
            block.windows(2).all(|w| w[0] == w[1]),
            "pixelate block not uniform"
        );
        let base_block: Vec<_> = (552..560)
            .flat_map(|x| (160..168).map(move |y| (x, y)))
            .map(|(x, y)| *base.get_pixel(x, y))
            .collect();
        assert!(
            base_block.windows(2).any(|w| w[0] != w[1]),
            "base unexpectedly uniform"
        );

        let cropped = render_to_image(
            &base,
            &annotations,
            Some(Rect::from_min_max(
                Pos2::new(100.0, 50.0),
                Pos2::new(500.0, 350.0),
            )),
        )
        .expect("render cropped");
        assert_eq!(cropped.dimensions(), (400, 300));

        if let Some(dir) = std::env::var_os("SCRANNOTATE_TEST_OUT") {
            let dir = std::path::PathBuf::from(dir);
            out.save(dir.join("render_full.png")).expect("save full");
            cropped
                .save(dir.join("render_cropped.png"))
                .expect("save cropped");
        }
    }
}
