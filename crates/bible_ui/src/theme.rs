//! Theme loading and adaptation.
//!
//! Built-in themes use gpui-component's ThemeSet schema. Zed themes use the
//! official Theme Family schema (name/author/themes/style), so they are
//! converted into the palette consumed by gpui-kit at startup. No Zed theme
//! file is modified.

use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::theme::{Theme, ThemeRegistry};
use gpui_kit::*;
use serde_json::{Value, json};

use crate::services::app_services;

const EVERFOREST_JSON: &str = include_str!("../assets/themes/everforest.json");
include!(concat!(env!("OUT_DIR"), "/embedded_zed_themes.rs"));

/// Theme applied when a saved theme has disappeared or cannot be loaded.
pub const DEFAULT_THEME: &str = "Everforest Dark Hard";
pub const CUSTOM_THEME_NAME: &str = "Моя тема";

/// Every independently configurable token used by BibleOWN's generated
/// palettes. Component-specific foregrounds and interaction states remain
/// separate on purpose, so the editor never silently changes a second colour.
pub const CUSTOM_THEME_FIELDS: &[(&str, &str)] = &[
    ("background", "#272E33"),
    ("foreground", "#D3C6AA"),
    ("caret", "#D3C6AA"),
    ("border", "#4F5B58"),
    ("ring", "#A7C080"),
    ("muted.background", "#323C41"),
    ("muted.foreground", "#9DA9A0"),
    ("popover.background", "#323C41"),
    ("popover.foreground", "#D3C6AA"),
    ("accent.background", "#40584B"),
    ("accent.foreground", "#F2F4EE"),
    ("primary.background", "#A7C080"),
    ("primary.foreground", "#1E2326"),
    ("primary.hover.background", "#B8CF94"),
    ("primary.active.background", "#C7D9A8"),
    ("secondary.background", "#374247"),
    ("secondary.foreground", "#D3C6AA"),
    ("secondary.hover.background", "#4B6154"),
    ("secondary.active.background", "#566D60"),
    ("danger.background", "#E67E80"),
    ("danger.foreground", "#1E2326"),
    ("warning.background", "#DBBC7F"),
    ("warning.foreground", "#1E2326"),
    ("info.background", "#7FBBB3"),
    ("info.foreground", "#1E2326"),
    ("success.background", "#A7C080"),
    ("success.foreground", "#1E2326"),
    ("link", "#83C092"),
    ("link.hover", "#A7D4B1"),
    ("selection.background", "#4B6557"),
    ("sidebar.background", "#2E383C"),
    ("sidebar.foreground", "#D3C6AA"),
    ("sidebar.border", "#4F5B58"),
    ("sidebar.accent.background", "#4B6557"),
    ("sidebar.accent.foreground", "#F2F4EE"),
    ("tab.background", "#2E383C"),
    ("tab.foreground", "#9DA9A0"),
    ("tab.active.background", "#272E33"),
    ("tab.active.foreground", "#D3C6AA"),
    ("tab_bar.background", "#323C41"),
    ("title_bar.background", "#374247"),
    ("title_bar.border", "#4F5B58"),
    ("status_bar.background", "#374247"),
    ("status_bar.border", "#4F5B58"),
    ("scrollbar.thumb.background", "#596763"),
    ("scrollbar.thumb.hover.background", "#83A598"),
    ("input.border", "#596763"),
    ("list.background", "#2E383C"),
    ("list.even.background", "#3A464A"),
    ("list.hover.background", "#506C5B"),
    ("list.active.background", "#665F46"),
    ("list.active.border", "#DBBC7F"),
    ("list.highlight.foreground", "#A7C080"),
    ("table.background", "#272E33"),
    ("table.head.background", "#374247"),
    ("table.head.foreground", "#D3C6AA"),
    ("table.even.background", "#323C41"),
    ("table.hover.background", "#506C5B"),
    ("table.active.background", "#665F46"),
    ("table.active.border", "#DBBC7F"),
    ("table.row.border", "#4F5B58"),
    ("base.red", "#E67E80"),
    ("base.yellow", "#DBBC7F"),
    ("base.green", "#A7C080"),
    ("base.cyan", "#83C092"),
    ("base.blue", "#7FBBB3"),
    ("base.magenta", "#D699B6"),
];

/// Register built-in and locally installed Zed themes, then restore the saved
/// selection. The import is intentionally offline: it reads local JSON files.
pub fn init(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(EVERFOREST_JSON) {
        log::error!("failed to load built-in themes: {error:#}");
        return;
    }

    load_zed_themes(cx);
    let (generated_background, generated_name) = {
        let settings = app_services(cx).settings.read();
        (
            settings.theme_generator_background.clone(),
            settings.theme_generator_name.clone(),
        )
    };
    if let Err(error) = register_generated_themes(&generated_background, &generated_name, cx) {
        log::warn!("cannot restore generated palettes: {error}");
    }
    let custom = app_services(cx).settings.read().custom_theme_colors.clone();
    if let Err(error) = register_custom_theme(&custom, cx) {
        log::warn!("cannot restore custom theme: {error}");
    }

    let saved = app_services(cx).settings.read().theme.clone();
    apply(&saved, cx);
}

/// Register (or replace) the user's exact semantic palette.
pub fn register_custom_theme(
    overrides: &std::collections::BTreeMap<String, String>,
    cx: &mut App,
) -> Result<(), String> {
    let mut colors = serde_json::Map::new();
    for &(key, fallback) in CUSTOM_THEME_FIELDS {
        let color = overrides.get(key).map(String::as_str).unwrap_or(fallback);
        colors.insert(key.into(), Value::String(normalise_hex(color)?));
    }
    let background = overrides
        .get("background")
        .map(String::as_str)
        .unwrap_or("#272E33");
    let mode = if rgb_to_hsl(rgb_from_hex(background)?).l < 0.5 {
        "dark"
    } else {
        "light"
    };
    let set = json!({
        "name": "Пользовательская тема BibleOWN",
        "author": "Пользователь BibleOWN",
        "themes": [{
            "name": CUSTOM_THEME_NAME,
            "mode": mode,
            "is_default": false,
            "shadow": false,
            "colors": colors
        }]
    });
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(&serde_json::to_string(&set).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())
}

/// Persist, register and immediately select the palette assembled in Settings.
pub fn save_custom_theme(
    colors: std::collections::BTreeMap<String, String>,
    cx: &mut App,
) -> Result<(), String> {
    register_custom_theme(&colors, cx)?;
    shell::settings::update(app_services(cx), |settings| {
        settings.custom_theme_colors = colors;
        settings.theme = CUSTOM_THEME_NAME.to_string();
    })
    .map_err(|error| error.to_string())?;
    apply(CUSTOM_THEME_NAME, cx);
    Ok(())
}

/// Registers five colour-wheel harmonies for a user supplied canvas colour.
/// The calculation starts in HSL (the input colour supplies luminance and, if
/// meaningful, hue); neutral backgrounds use a calm green-blue anchor. Each
/// scheme rotates the accent around the wheel while surfaces remain derived
/// from the exact supplied background. This is deliberately deterministic:
/// the same #RRGGBB always reconstructs the same accessible themes.
pub fn generate_from_background(
    background: &str,
    user_name: &str,
    cx: &mut App,
) -> Result<Vec<String>, String> {
    let user_name = normalise_user_theme_name(user_name);
    let names = register_generated_themes(background, &user_name, cx)?;
    let background = normalise_hex(background)?;
    shell::settings::update(app_services(cx), |settings| {
        settings.theme_generator_background = background;
        settings.theme_generator_name = user_name;
    })
    .map_err(|error| error.to_string())?;
    Ok(names)
}

fn register_generated_themes(
    background: &str,
    user_name: &str,
    cx: &mut App,
) -> Result<Vec<String>, String> {
    let background = rgb_from_hex(background)?;
    let user_name = normalise_user_theme_name(user_name);
    let base = rgb_to_hsl(background);
    // A neutral background has no reliable hue, so start from an editorial
    // blue-green rather than pretending grey has a colour direction.
    let anchor_hue = if base.s < 0.08 { 0.43 } else { base.h };
    let schemes: [(&str, &[f32]); 5] = [
        ("Аналоговая", &[0.08]),
        ("Комплементарная", &[0.50]),
        ("Раздельно-комплементарная", &[0.42, 0.58]),
        ("Триада", &[1.0 / 3.0, 2.0 / 3.0]),
        ("Тетрада", &[0.25, 0.50, 0.75]),
    ];
    let mut themes = Vec::with_capacity(schemes.len());
    let mut names = Vec::with_capacity(schemes.len());
    for (label, rotations) in schemes {
        // The first wheel point is primary; extra points feed semantic roles
        // so triadic/tetradic variants use the whole harmony rather than only
        // receiving a decorative name.
        let accents: Vec<Rgb> = rotations
            .iter()
            .map(|rotation| {
                hsl_to_rgb(Hsl {
                    h: wrap_hue(anchor_hue + rotation),
                    s: (0.48 + base.s * 0.20).clamp(0.48, 0.68),
                    l: if base.l < 0.5 { 0.64 } else { 0.40 },
                })
            })
            .collect();
        let name = format!("USER+{user_name} · {label}");
        let accent = accents[0];
        let info = *accents.get(1).unwrap_or(&rotate_hue(accent, 0.12));
        let warning = rotate_hue(accent, 0.10);
        let danger = rotate_hue(accent, 0.50);
        let is_dark = base.l < 0.5;
        let foreground = if is_dark {
            Rgb::new(0.88, 0.87, 0.82)
        } else {
            Rgb::new(0.13, 0.15, 0.16)
        };
        let muted_foreground = mix(foreground, background, if is_dark { 0.42 } else { 0.48 });
        let surface = mix(background, foreground, if is_dark { 0.075 } else { 0.045 });
        let raised = mix(background, foreground, if is_dark { 0.13 } else { 0.09 });
        let hover = mix(background, accent, if is_dark { 0.18 } else { 0.12 });
        let mut colors = serde_json::Map::new();
        for (key, colour) in [
            ("background", background),
            ("foreground", foreground),
            ("caret", foreground),
            ("border", raised),
            ("ring", accent),
            ("muted.background", surface),
            ("muted.foreground", muted_foreground),
            ("popover.background", surface),
            ("popover.foreground", foreground),
            ("accent.background", hover),
            ("accent.foreground", foreground),
            ("primary.background", accent),
            ("primary.foreground", background),
            ("primary.hover.background", mix(accent, foreground, 0.14)),
            ("primary.active.background", mix(accent, foreground, 0.25)),
            ("secondary.background", surface),
            ("secondary.foreground", foreground),
            ("secondary.hover.background", hover),
            ("secondary.active.background", raised),
            ("danger.background", danger),
            ("danger.foreground", background),
            ("warning.background", warning),
            ("warning.foreground", background),
            ("info.background", info),
            ("info.foreground", background),
            ("success.background", accent),
            ("success.foreground", background),
            ("link", info),
            ("link.hover", mix(info, foreground, 0.18)),
            ("selection.background", hover),
            ("sidebar.background", surface),
            ("sidebar.foreground", foreground),
            ("sidebar.border", raised),
            ("sidebar.accent.background", hover),
            ("sidebar.accent.foreground", foreground),
            ("tab.background", surface),
            ("tab.foreground", muted_foreground),
            ("tab.active.background", background),
            ("tab.active.foreground", foreground),
            ("tab_bar.background", surface),
            ("title_bar.background", raised),
            ("title_bar.border", surface),
            ("status_bar.background", raised),
            ("status_bar.border", surface),
            ("scrollbar.thumb.background", raised),
            ("scrollbar.thumb.hover.background", hover),
            ("input.border", raised),
            ("list.background", surface),
            ("list.hover.background", hover),
            ("list.active.background", raised),
            ("list.active.border", accent),
            ("list.highlight.foreground", accent),
            ("table.background", background),
            ("table.head.background", surface),
            ("table.head.foreground", muted_foreground),
            ("table.hover.background", hover),
            ("table.active.background", raised),
            ("table.active.border", accent),
            ("table.row.border", raised),
            ("base.red", danger),
            ("base.yellow", warning),
            ("base.green", accent),
            ("base.cyan", info),
            ("base.blue", info),
            ("base.magenta", rotate_hue(accent, 0.75)),
        ] {
            colors.insert(key.into(), Value::String(rgb_hex(colour)));
        }
        themes.push(json!({"name": name, "mode": if is_dark { "dark" } else { "light" }, "is_default": false, "shadow": false, "colors": colors}));
        names.push(name);
    }
    let set = json!({
        "name": format!("USER+{user_name}"),
        "author": "BibleOWN User",
        "themes": themes
    });
    ThemeRegistry::global_mut(cx)
        .load_themes_from_str(&serde_json::to_string(&set).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    Ok(names)
}

fn normalise_user_theme_name(input: &str) -> String {
    let trimmed = input.trim();
    let without_prefix = trimmed
        .strip_prefix("USER+")
        .or_else(|| trimmed.strip_prefix("user+"))
        .unwrap_or(trimmed)
        .trim();
    let cleaned = without_prefix
        .chars()
        .filter(|character| !character.is_control())
        .take(48)
        .collect::<String>();
    if cleaned.is_empty() {
        "Моя тема".to_string()
    } else {
        cleaned
    }
}

#[derive(Clone, Copy)]
struct Rgb {
    r: f32,
    g: f32,
    b: f32,
}
impl Rgb {
    const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }
}
#[derive(Clone, Copy)]
struct Hsl {
    h: f32,
    s: f32,
    l: f32,
}
fn mix(a: Rgb, b: Rgb, amount: f32) -> Rgb {
    Rgb::new(
        a.r + (b.r - a.r) * amount,
        a.g + (b.g - a.g) * amount,
        a.b + (b.b - a.b) * amount,
    )
}
fn wrap_hue(hue: f32) -> f32 {
    hue.rem_euclid(1.0)
}
fn rotate_hue(rgb: Rgb, amount: f32) -> Rgb {
    let mut hsl = rgb_to_hsl(rgb);
    hsl.h = wrap_hue(hsl.h + amount);
    hsl_to_rgb(hsl)
}
fn rgb_from_hex(input: &str) -> Result<Rgb, String> {
    let hex = normalise_hex(input)?;
    Ok(Rgb::new(
        u8::from_str_radix(&hex[1..3], 16).unwrap() as f32 / 255.,
        u8::from_str_radix(&hex[3..5], 16).unwrap() as f32 / 255.,
        u8::from_str_radix(&hex[5..7], 16).unwrap() as f32 / 255.,
    ))
}
fn normalise_hex(input: &str) -> Result<String, String> {
    let raw = input.trim().trim_start_matches('#');
    let expanded = match raw.len() {
        3 => raw.chars().flat_map(|c| [c, c]).collect(),
        6 => raw.to_owned(),
        _ => return Err("Нужен цвет формата #RRGGBB или #RGB.".into()),
    };
    if !expanded.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Цвет содержит не шестнадцатеричные символы.".into());
    };
    Ok(format!("#{}", expanded.to_ascii_uppercase()))
}
fn rgb_hex(rgb: Rgb) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (rgb.r.clamp(0., 1.) * 255.).round() as u8,
        (rgb.g.clamp(0., 1.) * 255.).round() as u8,
        (rgb.b.clamp(0., 1.) * 255.).round() as u8
    )
}
fn rgb_to_hsl(rgb: Rgb) -> Hsl {
    let max = rgb.r.max(rgb.g).max(rgb.b);
    let min = rgb.r.min(rgb.g).min(rgb.b);
    let d = max - min;
    let l = (max + min) / 2.;
    if d < f32::EPSILON {
        return Hsl { h: 0., s: 0., l };
    };
    let s = d / (1. - (2. * l - 1.).abs());
    let h = if max == rgb.r {
        ((rgb.g - rgb.b) / d).rem_euclid(6.)
    } else if max == rgb.g {
        (rgb.b - rgb.r) / d + 2.
    } else {
        (rgb.r - rgb.g) / d + 4.
    } / 6.;
    Hsl { h, s, l }
}
fn hsl_to_rgb(hsl: Hsl) -> Rgb {
    let c = (1. - (2. * hsl.l - 1.).abs()) * hsl.s;
    let x = c * (1. - ((hsl.h * 6.).rem_euclid(2.) - 1.).abs());
    let (r, g, b) = match (hsl.h * 6.).floor() as i32 {
        0 => (c, x, 0.),
        1 => (x, c, 0.),
        2 => (0., c, x),
        3 => (0., x, c),
        4 => (x, 0., c),
        _ => (c, 0., x),
    };
    let m = hsl.l - c / 2.;
    Rgb::new(r + m, g + m, b + m)
}

/// Names registered themes for the settings dropdown.
pub fn available(cx: &App) -> Vec<SharedString> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .map(|config| config.name.clone())
        .collect()
}

/// Apply one registered palette and the saved global typography.
pub fn apply(name: &str, cx: &mut App) {
    let registry = ThemeRegistry::global(cx);
    let config = registry
        .themes()
        .get(name)
        .or_else(|| registry.themes().get(DEFAULT_THEME))
        .cloned();
    match config {
        Some(config) => {
            let preserve_custom_interactions = config.name.as_ref() == CUSTOM_THEME_NAME;
            // Release the settings read-lock before mutating GPUI's app context.
            let (ui_font_family, ui_font_size) = {
                let settings = app_services(cx).settings.read();
                let font_size = if settings.ui_font_size.is_finite() {
                    settings.ui_font_size.clamp(10.0, 32.0)
                } else {
                    16.0
                };
                (settings.ui_font_family.clone(), font_size)
            };
            Theme::update(cx, |theme| {
                theme.apply_config(&config);
                // A modal backdrop dims the workspace in both light and dark palettes.
                theme.overlay = gpui_kit::hsla(0., 0., 0., 0.48);
                // A single quiet scrollbar treatment across reading, lists,
                // popups and editors. The transparent track does not add a
                // visual gutter; the rounded thumb stays visible and gains
                // contrast only on hover/drag.
                theme.scrollbar_mode = ScrollbarMode::Always;
                theme.scrollbar = theme.background.opacity(0.0);
                theme.scrollbar_thumb = theme.foreground.opacity(0.22);
                theme.scrollbar_thumb_hover = theme.accent.opacity(0.62);
                if !preserve_custom_interactions {
                    // Imported themes often use interaction colours that are
                    // only a few percent away from their background. BibleOWN
                    // has dense module/note lists, so hover, selection and row
                    // striping need to remain recognisable in peripheral vision.
                    theme.list_even = theme.foreground.opacity(0.14);
                    theme.list_hover = theme.accent.opacity(0.40);
                    theme.list_active = theme.warning.opacity(0.30);
                    theme.list_active_border = theme.warning.opacity(0.90);
                    theme.table_even = theme.foreground.opacity(0.12);
                    theme.table_hover = theme.accent.opacity(0.40);
                    theme.table_active = theme.warning.opacity(0.30);
                    theme.sidebar_accent = theme.accent.opacity(0.36);
                    theme.secondary_hover = theme.accent.opacity(0.32);
                    theme.selection = theme.accent.opacity(0.38);
                }
                if !ui_font_family.is_empty() {
                    theme.font_family = ui_font_family.into();
                }
                theme.font_size = px(ui_font_size);
            });
        }
        None => log::warn!("theme {name:?} not found in registry, keeping current"),
    }
}

/// Re-apply the currently selected palette so a changed interface font can be
/// restored to the proper theme defaults when the user chooses "По умолчанию".
pub fn refresh_typography(cx: &mut App) {
    let name = app_services(cx).settings.read().theme.clone();
    apply(&name, cx);
}

/// Persist then apply the selected theme.
pub fn select(name: &str, cx: &mut App) {
    let name = name.to_string();
    match shell::settings::update(app_services(cx), |settings| {
        settings.theme = name.clone();
    }) {
        Ok(()) => {
            crate::settings::set_save_status(
                "Последнее изменение успешно записано в settings.json.".to_string(),
            );
            apply(&name, cx);
        }
        Err(error) => {
            let message = format!("Не удалось сохранить тему в settings.json: {error:#}");
            log::error!("{message}");
            crate::settings::set_save_status(message);
        }
    }
    crate::navigation::notify_settings_changed(cx);
}

fn load_zed_themes(cx: &mut App) {
    let mut loaded = 0usize;
    for (name, contents) in EMBEDDED_ZED_THEMES {
        loaded += register_zed_theme(cx, name, contents);
    }
    for source in shell::themes::external_sources() {
        loaded += register_zed_theme(cx, &source.path.display().to_string(), &source.contents);
    }

    if loaded > 0 {
        log::info!("loaded {loaded} embedded or locally installed Zed-compatible themes");
    }
}

fn register_zed_theme(cx: &mut App, name: &str, contents: &str) -> usize {
    let family: Value = match parse_zed_json(contents) {
        Ok(value) => value,
        Err(error) => {
            log::warn!("invalid theme JSON {name}: {error}");
            return 0;
        }
    };
    let converted = match convert_zed_theme_family(&family, EVERFOREST_JSON) {
        Ok(converted) => converted,
        Err(error) => {
            log::debug!("skip non-Zed theme {name}: {error:#}");
            return 0;
        }
    };
    let count = converted["themes"]
        .as_array()
        .map_or(0, |themes| themes.len());
    let converted_json = match serde_json::to_string(&converted) {
        Ok(json) => json,
        Err(error) => {
            log::warn!("cannot serialize converted theme {name}: {error}");
            return 0;
        }
    };
    match ThemeRegistry::global_mut(cx).load_themes_from_str(&converted_json) {
        Ok(()) => count,
        Err(error) => {
            log::warn!("cannot register Zed theme {name}: {error:#}");
            0
        }
    }
}

/// Zed accepts JSON with JavaScript-style comments. Theme extensions in its
/// catalogue use that form, while `serde_json` intentionally does not.
fn parse_zed_json(contents: &str) -> serde_json::Result<Value> {
    match serde_json::from_str(contents) {
        Ok(value) => Ok(value),
        Err(_) => serde_json::from_str(&strip_json_comments(contents)),
    }
}

fn strip_json_comments(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if in_string {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            output.push(ch);
        } else if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for comment in chars.by_ref() {
                if comment == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for comment in chars.by_ref() {
                if comment == '\n' {
                    output.push('\n');
                }
                if previous == '*' && comment == '/' {
                    break;
                }
                previous = comment;
            }
        } else {
            output.push(ch);
        }
    }
    output
}

/// Convert official Zed Theme Family JSON into the color palette format used
/// by gpui-kit. Syntax scopes and terminal-specific colors are intentionally
/// ignored because BibleOWN currently has no code editor or terminal surface.
fn convert_zed_theme_family(family: &Value, built_in: &str) -> anyhow::Result<Value> {
    let family_name = family
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Zed theme");
    let author = family.get("author").and_then(Value::as_str).unwrap_or("");
    let themes = family
        .get("themes")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("missing Zed theme family `themes` array"))?;

    let built_in: Value = serde_json::from_str(built_in)?;
    let mut converted_themes = Vec::with_capacity(themes.len());

    for theme in themes {
        let name = theme
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| anyhow::anyhow!("theme entry has no name"))?;
        let mode = theme
            .get("appearance")
            .and_then(Value::as_str)
            .filter(|appearance| *appearance == "light" || *appearance == "dark")
            .unwrap_or("dark");
        let style = theme
            .get("style")
            .ok_or_else(|| anyhow::anyhow!("theme {name:?} has no style object"))?;

        let fallback = built_in["themes"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["mode"] == mode))
            .or_else(|| {
                built_in["themes"]
                    .as_array()
                    .and_then(|items| items.first())
            })
            .ok_or_else(|| anyhow::anyhow!("built-in fallback palette is missing"))?;
        let mut colors = fallback["colors"].as_object().cloned().unwrap_or_default();

        let mappings: &[(&str, &[&str])] = &[
            ("background", &["background", "editor.background"]),
            ("foreground", &["text", "editor.foreground"]),
            ("caret", &["editor.foreground", "text"]),
            ("border", &["border", "pane_group.border"]),
            (
                "ring",
                &["border.focused", "pane.focused_border", "border.selected"],
            ),
            (
                "muted.background",
                &["surface.background", "panel.background"],
            ),
            ("muted.foreground", &["text.muted", "text.disabled"]),
            (
                "popover.background",
                &["elevated_surface.background", "surface.background"],
            ),
            ("popover.foreground", &["text"]),
            (
                "accent.background",
                &["element.selected", "element.active", "accent"],
            ),
            ("accent.foreground", &["text.accent", "text"]),
            (
                "primary.background",
                &["accent", "element.selected", "element.background"],
            ),
            ("primary.foreground", &["background"]),
            ("primary.hover.background", &["element.hover", "accent"]),
            (
                "primary.active.background",
                &["element.active", "element.selected"],
            ),
            (
                "secondary.background",
                &["element.background", "surface.background"],
            ),
            ("secondary.foreground", &["text"]),
            (
                "secondary.hover.background",
                &["element.hover", "ghost_element.hover"],
            ),
            (
                "secondary.active.background",
                &["element.active", "ghost_element.active"],
            ),
            ("danger.background", &["error.background", "error"]),
            ("danger.foreground", &["text"]),
            ("warning.background", &["warning.background", "warning"]),
            ("warning.foreground", &["text"]),
            ("info.background", &["info.background", "info"]),
            ("info.foreground", &["text"]),
            ("success.background", &["success.background", "success"]),
            ("success.foreground", &["text"]),
            ("link", &["text.accent", "accent"]),
            ("link.hover", &["link_text.hover", "text.accent"]),
            (
                "selection.background",
                &[
                    "search.match_background",
                    "editor.document_highlight.read_background",
                    "element.selected",
                ],
            ),
            (
                "sidebar.background",
                &["panel.background", "surface.background"],
            ),
            ("sidebar.foreground", &["text"]),
            ("sidebar.border", &["border"]),
            (
                "sidebar.accent.background",
                &["element.hover", "ghost_element.hover"],
            ),
            ("sidebar.accent.foreground", &["text"]),
            (
                "tab.background",
                &[
                    "tab.inactive_background",
                    "tab_bar.background",
                    "surface.background",
                ],
            ),
            ("tab.foreground", &["text.muted"]),
            (
                "tab.active.background",
                &["tab.active_background", "surface.background"],
            ),
            ("tab.active.foreground", &["text"]),
            (
                "tab_bar.background",
                &["tab_bar.background", "panel.background"],
            ),
            (
                "title_bar.background",
                &["title_bar.background", "background"],
            ),
            ("title_bar.border", &["border"]),
            (
                "status_bar.background",
                &["status_bar.background", "panel.background"],
            ),
            (
                "scrollbar.thumb.background",
                &["scrollbar.thumb.background"],
            ),
            (
                "scrollbar.thumb.hover.background",
                &[
                    "scrollbar.thumb.hover_background",
                    "scrollbar.thumb.background",
                ],
            ),
            ("input.border", &["border"]),
            (
                "list.background",
                &["panel.background", "surface.background"],
            ),
            (
                "list.hover.background",
                &["element.hover", "ghost_element.hover"],
            ),
            (
                "list.active.background",
                &["element.selected", "element.active"],
            ),
            (
                "list.active.border",
                &["border.selected", "pane.focused_border"],
            ),
            ("list.highlight.foreground", &["text.accent"]),
            ("table.background", &["editor.background", "background"]),
            (
                "table.head.background",
                &["surface.background", "panel.background"],
            ),
            ("table.head.foreground", &["text.muted"]),
            ("table.hover.background", &["element.hover"]),
            ("table.active.background", &["element.selected"]),
            ("table.active.border", &["border.selected"]),
            ("table.row.border", &["border"]),
        ];

        for (target, sources) in mappings {
            if let Some(color) = first_style_color(style, sources) {
                colors.insert((*target).to_string(), Value::String(color.to_string()));
            }
        }

        // Import the Zed ANSI palette as the semantic accent palette used by
        // gpui-kit charts, annotations and status indicators.
        for (target, source) in [
            ("base.red", "terminal.ansi.red"),
            ("base.yellow", "terminal.ansi.yellow"),
            ("base.green", "terminal.ansi.green"),
            ("base.cyan", "terminal.ansi.cyan"),
            ("base.blue", "terminal.ansi.blue"),
            ("base.magenta", "terminal.ansi.magenta"),
        ] {
            if let Some(color) = style.get(source).and_then(Value::as_str) {
                colors.insert(target.to_string(), Value::String(color.to_string()));
            }
        }

        converted_themes.push(json!({
            "name": name,
            "mode": mode,
            "is_default": false,
            "shadow": false,
            "colors": colors,
        }));
    }

    anyhow::ensure!(
        !converted_themes.is_empty(),
        "theme family contains no themes"
    );
    Ok(json!({
        "name": family_name,
        "author": author,
        "themes": converted_themes,
    }))
}

fn first_style_color<'a>(style: &'a Value, candidates: &[&str]) -> Option<&'a str> {
    candidates.iter().find_map(|key| {
        style
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
    })
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::theme::ThemeSet;
    use serde_json::json;

    use super::{
        CUSTOM_THEME_FIELDS, DEFAULT_THEME, EMBEDDED_ZED_THEMES, EVERFOREST_JSON,
        convert_zed_theme_family, hsl_to_rgb, normalise_hex, normalise_user_theme_name,
        parse_zed_json, rgb_from_hex, rgb_hex, rgb_to_hsl, rotate_hue, wrap_hue,
    };

    #[test]
    fn embedded_theme_set_parses_with_six_themes() {
        let set: ThemeSet = serde_json::from_str(EVERFOREST_JSON).unwrap();
        assert_eq!(set.themes.len(), 6);
        let dark = set
            .themes
            .iter()
            .filter(|theme| theme.mode.is_dark())
            .count();
        assert_eq!(dark, 3);
        assert!(set.themes.iter().any(|theme| theme.name == DEFAULT_THEME));
    }

    #[test]
    fn zed_theme_family_is_converted_to_gpui_palette() {
        let zed = json!({
            "name": "Example Pack",
            "author": "BibleOWN test",
            "themes": [{
                "name": "Example Night",
                "appearance": "dark",
                "style": {
                    "background": "#101010",
                    "text": "#f0f0f0",
                    "text.muted": "#888888",
                    "accent": "#66ccaa",
                    "border": "#333333",
                    "element.hover": "#222222"
                }
            }]
        });

        let converted = convert_zed_theme_family(&zed, EVERFOREST_JSON).unwrap();
        assert_eq!(converted["themes"][0]["colors"]["background"], "#101010");
        let set: ThemeSet = serde_json::from_value(converted).unwrap();
        assert_eq!(set.themes.len(), 1);
        assert_eq!(set.themes[0].name, "Example Night");
    }

    #[test]
    fn bundled_zed_theme_files_are_convertible() {
        assert!(!EMBEDDED_ZED_THEMES.is_empty());
        let mut converted_count = 0usize;
        for (name, contents) in EMBEDDED_ZED_THEMES {
            let family = parse_zed_json(contents).unwrap_or_else(|error| panic!("{name}: {error}"));
            match convert_zed_theme_family(&family, EVERFOREST_JSON) {
                Ok(converted) => {
                    converted_count += converted["themes"].as_array().map_or(0, Vec::len);
                }
                Err(error) => {
                    // The downloaded Zed extension directory can also contain
                    // extension metadata JSON; only theme-family files count.
                    assert!(
                        !name.replace('\\', "/").contains("/themes/"),
                        "theme file {name} cannot be converted: {error:#}"
                    );
                }
            }
        }
        assert!(converted_count > 0);
    }

    #[test]
    fn generated_theme_math_roundtrips_and_preserves_background() {
        let background = rgb_from_hex("#272e33").unwrap();
        assert_eq!(rgb_hex(background), "#272E33");
        let hsl = rgb_to_hsl(background);
        let restored = hsl_to_rgb(hsl);
        assert!((restored.r - background.r).abs() < 0.001);
        assert!((restored.g - background.g).abs() < 0.001);
        assert!((restored.b - background.b).abs() < 0.001);
        // Rotations are a colour-wheel operation, not arbitrary RGB shifts.
        assert!((rgb_to_hsl(rotate_hue(background, 0.5)).h - wrap_hue(hsl.h + 0.5)).abs() < 0.001);
    }

    #[test]
    fn generated_theme_input_accepts_short_and_long_hex() {
        assert_eq!(normalise_hex("#abc").unwrap(), "#AABBCC");
        assert_eq!(normalise_hex("272e33").unwrap(), "#272E33");
        assert!(normalise_hex("not-a-colour").is_err());
    }

    #[test]
    fn custom_theme_editor_has_unique_valid_tokens() {
        let mut keys = std::collections::HashSet::new();
        for &(key, color) in CUSTOM_THEME_FIELDS {
            assert!(keys.insert(key), "duplicate custom theme token: {key}");
            assert_eq!(normalise_hex(color).unwrap(), color);
        }
        assert!(keys.contains("background"));
        assert!(keys.contains("list.even.background"));
        assert!(keys.contains("list.hover.background"));
        assert!(keys.contains("list.active.background"));
        assert!(keys.contains("scrollbar.thumb.hover.background"));
    }

    #[test]
    fn generated_theme_names_have_one_user_prefix() {
        assert_eq!(normalise_user_theme_name("  Study  "), "Study");
        assert_eq!(normalise_user_theme_name("USER+Study"), "Study");
        assert_eq!(normalise_user_theme_name("  "), "Моя тема");
    }
}
