//! Persisted user preferences: a tiny `key=value` file in the same state
//! directory as the portal restore token ([`state_dir`]). Stores the
//! recently-used color palette and the toolbar's UI size (Small/Medium/
//! Large), and — only once the user has deliberately adjusted them — the
//! stroke width and font size. Untouched sizes stay resolution-scaled
//! defaults and are never written, so a small capture's defaults can't leak
//! into a 4K session; the UI size has no such resolution-scaled default, so
//! it's written unconditionally, every time.

use std::path::PathBuf;

use eframe::egui::Color32;

use crate::annotate::Style;
use crate::ui::UiScale;

/// `$XDG_STATE_HOME/scrannotate` (default `~/.local/state/scrannotate`).
#[cfg(all(unix, not(target_os = "macos")))]
pub fn state_dir() -> Option<PathBuf> {
    let root = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".local/state")))?;
    Some(root.join("scrannotate"))
}

/// `~/Library/Application Support/scrannotate` on macOS,
/// `%LOCALAPPDATA%\scrannotate` on Windows.
#[cfg(any(target_os = "macos", windows))]
pub fn state_dir() -> Option<PathBuf> {
    Some(dirs::data_local_dir()?.join("scrannotate"))
}

fn prefs_path() -> Option<PathBuf> {
    Some(state_dir()?.join("prefs"))
}

#[derive(Default)]
pub struct Prefs {
    /// Recently-used palette, newest first (the head is the active color).
    pub palette: Option<Vec<Color32>>,
    pub width: Option<f32>,
    pub font_size: Option<f32>,
    /// Toolbar density. Unlike `width`/`font_size`, always written once the
    /// user has touched it — there's no resolution-scaled default it could
    /// clobber.
    pub ui_scale: Option<UiScale>,
    /// Export border toggle. Written unconditionally alongside `ui_scale`,
    /// for the same reason: it is a deliberate choice with no
    /// resolution-scaled default it could clobber.
    pub border: Option<bool>,
}

fn parse_color(hex: &str) -> Option<Color32> {
    let hex = hex.trim();
    if hex.len() != 8 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok();
    Some(Color32::from_rgba_unmultiplied(byte(0)?, byte(2)?, byte(4)?, byte(6)?))
}

fn format_color(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    format!("{r:02x}{g:02x}{b:02x}{a:02x}")
}

pub fn load() -> Prefs {
    let mut prefs = Prefs::default();
    let Some(path) = prefs_path() else { return prefs };
    let Ok(contents) = std::fs::read_to_string(path) else { return prefs };
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key.trim() {
            "palette" => {
                let colors: Vec<Color32> = value.split(',').filter_map(parse_color).collect();
                if !colors.is_empty() {
                    prefs.palette = Some(colors);
                }
            }
            "width" => {
                prefs.width = value.trim().parse().ok().filter(|w| (1.0..=64.0).contains(w));
            }
            "font_size" => {
                prefs.font_size =
                    value.trim().parse().ok().filter(|s| (6.0..=400.0).contains(s));
            }
            "ui_scale" => {
                prefs.ui_scale = UiScale::parse(value);
            }
            "border" => {
                prefs.border = match value.trim() {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
            _ => {}
        }
    }
    prefs
}

/// `style: None` keeps sizes out of the file (they stay session defaults).
pub fn save(palette: &[Color32], style: Option<&Style>, ui_scale: UiScale, border: bool) {
    let Some(path) = prefs_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let palette_line =
        palette.iter().map(|c| format_color(*c)).collect::<Vec<_>>().join(",");
    let mut contents = format!(
        "palette={palette_line}\nui_scale={}\nborder={border}\n",
        ui_scale.as_str()
    );
    if let Some(style) = style {
        contents.push_str(&format!("width={}\nfont_size={}\n", style.width, style.font_size));
    }
    if let Err(err) = std::fs::write(&path, contents) {
        eprintln!("warning: could not persist preferences: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_roundtrip() {
        let c = Color32::from_rgba_unmultiplied(0xe0, 0x2d, 0x2d, 0xff);
        assert_eq!(parse_color(&format_color(c)), Some(c));
        assert_eq!(parse_color("nonsense"), None);
        assert_eq!(parse_color("e02d2d"), None); // rgb without alpha
    }
}
