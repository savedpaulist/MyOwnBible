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

/// Register built-in and locally installed Zed themes, then restore the saved
/// selection. The import is intentionally offline: it reads local JSON files.
pub fn init(cx: &mut App) {
    if let Err(error) = ThemeRegistry::global_mut(cx).load_themes_from_str(EVERFOREST_JSON) {
        log::error!("failed to load built-in themes: {error:#}");
        return;
    }

    load_zed_themes(cx);

    let saved = app_services(cx).settings.read().theme.clone();
    apply(&saved, cx);
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
        DEFAULT_THEME, EMBEDDED_ZED_THEMES, EVERFOREST_JSON, convert_zed_theme_family,
        parse_zed_json,
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
}
