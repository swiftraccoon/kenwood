// SPDX-FileCopyrightText: 2026 Swift Raccoon
// SPDX-License-Identifier: GPL-2.0-or-later OR GPL-3.0-or-later

//! Instrument design system: colour tokens, typography, and the app
//! `Style`.
//!
//! Palette discipline: [`LIVE`] amber marks live RF activity only,
//! [`TX`] red marks "you are radiating" only, [`OK`] green marks link
//! health and audio meters only. No token is reused outside its role.

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, Margin, Rounding, Stroke,
    TextStyle, Vec2,
};

/// Window base fill.
pub(crate) const BG_WINDOW: Color32 = Color32::from_rgb(0x13, 0x14, 0x17);
/// Inset surfaces (TX strip, text-edit backgrounds).
pub(crate) const BG_INSET: Color32 = Color32::from_rgb(0x10, 0x11, 0x14);
/// Buttons and interactive fills.
pub(crate) const BG_CONTROL: Color32 = Color32::from_rgb(0x23, 0x25, 0x29);
/// Hover brightening of [`BG_CONTROL`].
pub(crate) const BG_CONTROL_HOVER: Color32 = Color32::from_rgb(0x2A, 0x2C, 0x31);
/// 1 px separators.
pub(crate) const HAIRLINE: Color32 = Color32::from_rgb(0x26, 0x27, 0x2C);
/// Control outlines (slightly brighter than [`HAIRLINE`]).
pub(crate) const BORDER: Color32 = Color32::from_rgb(0x2F, 0x31, 0x37);
/// Headings and callsigns at rest.
pub(crate) const TEXT_PRIMARY: Color32 = Color32::from_rgb(0xE8, 0xE8, 0xEA);
/// Labels and metadata.
pub(crate) const TEXT_SECONDARY: Color32 = Color32::from_rgb(0x9A, 0x9B, 0xA3);
/// Hints and timestamps.
pub(crate) const TEXT_TERTIARY: Color32 = Color32::from_rgb(0x63, 0x66, 0x6E);
/// Live-RF amber: active RX callsign, page-toggle accent, favorites.
pub(crate) const LIVE: Color32 = Color32::from_rgb(0xFF, 0xB3, 0x40);
/// Healthy-link green: status dot, mic/RX meters.
pub(crate) const OK: Color32 = Color32::from_rgb(0x34, 0xC7, 0x59);
/// On-air red: active TRANSMIT fill and hero tint only.
pub(crate) const TX: Color32 = Color32::from_rgb(0xFF, 0x45, 0x3A);
/// Error red: log errors and the error strip.
pub(crate) const ERR: Color32 = Color32::from_rgb(0xFF, 0x5F, 0x57);
/// Event blue: log event lines.
pub(crate) const INFO: Color32 = Color32::from_rgb(0x64, 0xA7, 0xFF);

/// Medium-weight proportional family (section headings, buttons).
pub(crate) fn family_medium() -> FontFamily {
    FontFamily::Name("inter-medium".into())
}

/// Medium-weight monospace family (hero callsign, header identity).
pub(crate) fn family_mono_medium() -> FontFamily {
    FontFamily::Name("mono-medium".into())
}

/// Apply the Instrument fonts + style to the whole app. Call once at
/// startup from `App::new`.
pub(crate) fn apply(ctx: &egui::Context) {
    ctx.set_fonts(font_definitions());
    ctx.set_style(style());
}

/// Small uppercase section label ("NOW RECEIVING", "HEARD").
/// egui has no letter-spacing; uppercase + small + tertiary carries
/// the label style instead.
pub(crate) fn section_label(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .text_style(TextStyle::Small)
        .color(TEXT_TERTIARY)
}

/// Thin horizontal level meter: a [`HAIRLINE`] track with a `color`
/// fill for `fraction` (clamped to `0.0..=1.0`) of its width.
pub(crate) fn level_meter(ui: &mut egui::Ui, fraction: f32, color: Color32) {
    let desired = Vec2::new(ui.available_width().max(40.0), 4.0);
    let (rect, _response) = ui.allocate_exact_size(desired, egui::Sense::hover());
    let painter = ui.painter();
    let _track = painter.rect_filled(rect, Rounding::same(2.0), HAIRLINE);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    let _fill = painter.rect_filled(fill, Rounding::same(2.0), color);
}

/// Font stack: `Inter` for UI text, `JetBrains Mono` for data, with
/// the egui defaults retained as glyph fallback (emoji / symbols).
fn font_definitions() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    // Fallback stacks BEFORE we prepend our faces.
    let prop_fallback = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let mono_fallback = fonts
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    let faces: [(&str, &'static [u8]); 4] = [
        ("inter", include_bytes!("../assets/Inter-Regular.ttf")),
        ("inter-medium", include_bytes!("../assets/Inter-Medium.ttf")),
        (
            "jbmono",
            include_bytes!("../assets/JetBrainsMono-Regular.ttf"),
        ),
        (
            "jbmono-medium",
            include_bytes!("../assets/JetBrainsMono-Medium.ttf"),
        ),
    ];
    for (name, bytes) in faces {
        let _prev = fonts
            .font_data
            .insert((*name).to_owned(), FontData::from_static(bytes));
    }

    let stack = |first: &str, fallback: &[String]| -> Vec<String> {
        let mut v = vec![first.to_owned()];
        v.extend(fallback.iter().cloned());
        v
    };
    let _prev = fonts
        .families
        .insert(FontFamily::Proportional, stack("inter", &prop_fallback));
    let _prev = fonts
        .families
        .insert(FontFamily::Monospace, stack("jbmono", &mono_fallback));
    let _prev = fonts.families.insert(
        FontFamily::Name("inter-medium".into()),
        stack("inter-medium", &prop_fallback),
    );
    let _prev = fonts.families.insert(
        FontFamily::Name("mono-medium".into()),
        stack("jbmono-medium", &mono_fallback),
    );
    fonts
}

/// The Instrument `Style`: dark visuals mapped to the palette tokens,
/// 4 pt spacing grid, radius-8 controls, hairline strokes.
fn style() -> egui::Style {
    let mut style = egui::Style {
        visuals: egui::Visuals::dark(),
        ..egui::Style::default()
    };
    let v = &mut style.visuals;
    v.panel_fill = BG_WINDOW;
    v.window_fill = BG_WINDOW;
    v.extreme_bg_color = BG_INSET;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_rounding = Rounding::same(12.0);
    v.hyperlink_color = INFO;
    v.error_fg_color = ERR;
    v.warn_fg_color = LIVE;
    v.selection.bg_fill = Color32::from_rgba_unmultiplied(0xFF, 0xB3, 0x40, 0x30);
    v.selection.stroke = Stroke::new(1.0, LIVE);

    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, HAIRLINE);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_SECONDARY);
    v.widgets.inactive.bg_fill = BG_CONTROL;
    v.widgets.inactive.weak_bg_fill = BG_CONTROL;
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.hovered.bg_fill = BG_CONTROL_HOVER;
    v.widgets.hovered.weak_bg_fill = BG_CONTROL_HOVER;
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, TEXT_TERTIARY);
    v.widgets.hovered.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.active.bg_fill = BG_CONTROL_HOVER;
    v.widgets.active.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    v.widgets.open.bg_fill = BG_CONTROL_HOVER;
    v.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = Rounding::same(8.0);
    }

    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(14.0, 7.0);
    style.spacing.menu_margin = Margin::same(12.0);
    style.spacing.window_margin = Margin::same(16.0);

    let _prev = style
        .text_styles
        .insert(TextStyle::Body, FontId::new(12.0, FontFamily::Proportional));
    let _prev = style.text_styles.insert(
        TextStyle::Button,
        FontId::new(12.5, FontFamily::Proportional),
    );
    let _prev = style.text_styles.insert(
        TextStyle::Small,
        FontId::new(10.0, FontFamily::Proportional),
    );
    let _prev = style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(15.0, family_medium()));
    let _prev = style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.0, FontFamily::Monospace),
    );
    // Named style for the hero callsign, the loudest element in the
    // app; the theme owns its size so panels can't drift.
    let _prev = style.text_styles.insert(
        TextStyle::Name("hero".into()),
        FontId::new(44.0, family_mono_medium()),
    );
    style
}

/// The named text style for the hero callsign registered by
/// [`style`].
pub(crate) fn hero_text_style() -> TextStyle {
    TextStyle::Name("hero".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn font_definitions_register_all_four_faces() {
        let fonts = font_definitions();
        for name in ["inter", "inter-medium", "jbmono", "jbmono-medium"] {
            assert!(
                fonts.font_data.contains_key(name),
                "face {name} must be registered"
            );
        }
    }

    #[test]
    fn instrument_faces_lead_every_family() -> TestResult {
        let fonts = font_definitions();
        let first = |family: &FontFamily| -> Result<String, String> {
            fonts
                .families
                .get(family)
                .and_then(|list| list.first().cloned())
                .ok_or_else(|| format!("family {family:?} missing"))
        };
        assert_eq!(first(&FontFamily::Proportional)?, "inter");
        assert_eq!(first(&FontFamily::Monospace)?, "jbmono");
        assert_eq!(first(&family_medium())?, "inter-medium");
        assert_eq!(first(&family_mono_medium())?, "jbmono-medium");
        Ok(())
    }

    #[test]
    fn style_maps_panel_fill_to_window_token() {
        let style = style();
        assert_eq!(style.visuals.panel_fill, BG_WINDOW);
        assert_eq!(style.visuals.extreme_bg_color, BG_INSET);
    }
}
