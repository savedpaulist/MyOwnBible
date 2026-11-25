//! Shared helpers for application and panel typography.

use gpui_kit::*;

/// The family used by side panels. An empty value means that the global UI
/// family remains inherited from the workspace root.
pub fn panel_font_family(cx: &App) -> String {
    crate::services::app_services(cx)
        .settings
        .read()
        .side_panel_font_family
        .clone()
}

/// Typography size for side panels. Existing per-component sizes remain
/// proportional, while the settings expose a predictable 14 px baseline.
pub fn panel_font_size(cx: &App, base_px: f32) -> f32 {
    let settings = crate::services::app_services(cx).settings.read();
    let interface = if settings.interface_scale.is_finite() {
        settings.interface_scale.clamp(0.8, 1.6)
    } else {
        1.0
    };
    let panel_scale = if settings.side_panel_font_scale.is_finite() {
        settings.side_panel_font_scale.clamp(0.8, 1.6)
    } else {
        1.0
    };
    let panel_base = if settings.side_panel_font_size.is_finite() {
        // The settings selector exposes 10..32 px. Keep the whole range
        // reachable instead of silently capping larger values near 24 px.
        (settings.side_panel_font_size / 14.0).clamp(10.0 / 14.0, 32.0 / 14.0)
    } else {
        1.0
    };

    // Профиль панелей независим от размера текста интерфейса: изменение
    // одного профиля не должно неожиданно менять два остальных.
    base_px * interface * panel_scale * panel_base
}

pub(crate) fn row_background(index: usize, cx: &App) -> Hsla {
    use gpui_kit::component::ActiveTheme as _;
    if index % 2 == 0 {
        transparent_black()
    } else {
        // Use the dedicated stripe token rather than a muted surface. The
        // theme adapter deliberately keeps this distinct from both hover and
        // active selection, including for imported Zed themes.
        cx.theme().list_even
    }
}

/// Shared, deliberately high-contrast pointer state for hand-built rows.
pub(crate) fn hover_background(cx: &App) -> Hsla {
    use gpui_kit::component::ActiveTheme as _;
    cx.theme().list_hover
}

pub(crate) fn icon_size(cx: &App) -> Pixels {
    let settings = crate::services::app_services(cx).settings.read();
    px(settings.icon_size.clamp(12., 24.) * settings.interface_scale.clamp(0.8, 1.6))
}

pub(crate) fn dictionary_font_family(cx: &App) -> String {
    crate::services::app_services(cx)
        .settings
        .read()
        .dictionary_font_family
        .clone()
}

pub(crate) fn dictionary_font_size(cx: &App, base: f32) -> f32 {
    let settings = crate::services::app_services(cx).settings.read();
    profile_font_size(
        base,
        settings.dictionary_font_size,
        settings.interface_scale,
    )
}

pub(crate) fn left_font_family(cx: &App) -> String {
    crate::services::app_services(cx)
        .settings
        .read()
        .left_panel_font_family
        .clone()
}

pub(crate) fn left_font_size(cx: &App, base: f32) -> f32 {
    let settings = crate::services::app_services(cx).settings.read();
    profile_font_size(
        base,
        settings.left_panel_font_size,
        settings.interface_scale,
    )
}

fn profile_font_size(base: f32, size: f32, scale: f32) -> f32 {
    let size = if size.is_finite() && size > 0. {
        size.clamp(10., 32.)
    } else {
        14.
    };
    let scale = if scale.is_finite() {
        scale.clamp(0.8, 1.6)
    } else {
        1.
    };
    base * (size / 14.) * scale
}
