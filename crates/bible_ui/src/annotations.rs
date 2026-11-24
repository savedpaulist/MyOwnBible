//! UI-сторона аннотаций: имена палитры (`bible_domain::annotations::PALETTE`)
//! → цвета текущей темы. Домен хранит только стабильные имена («yellow»),
//! конкретные HSL — забота оформления (architecture.md §3.6).

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{App, Hsla};

/// Русская подпись цвета для меню.
pub fn label(name: &str) -> &str {
    match name {
        "yellow" => "жёлтый",
        "green" => "зелёный",
        "blue" => "синий",
        "pink" => "розовый",
        "orange" => "оранжевый",
        "purple" => "фиолетовый",
        other => other,
    }
}

/// Базовый цвет метки берётся из палитры темы; пользовательский выбор
/// сохраняется на конкретной аннотации, а не меняет глобальную палитру.
pub fn base(name: &str, cx: &App) -> Hsla {
    if name.starts_with('#') {
        return parse_hex(name).unwrap_or_else(|| cx.theme().accent);
    }

    let theme = cx.theme();
    match name {
        "yellow" => theme.yellow,
        "green" => theme.green,
        "blue" => theme.blue,
        "pink" => theme.red,
        "orange" => theme.warning,
        "purple" => theme.magenta,
        _ => theme.accent,
    }
}

/// «#rrggbb» или «#rgb» → Hsla. Невалидная строка — None (мягкий откат,
/// этап 22): настройки правятся руками, битый цвет не должен ломать UI.
pub fn parse_hex(input: &str) -> Option<Hsla> {
    let digits = input.trim().strip_prefix('#')?;
    if !digits.bytes().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }
    let expand = |pair: &str| u8::from_str_radix(pair, 16).ok();
    let (r, g, b) = match digits.len() {
        3 => {
            let chars: Vec<char> = digits.chars().collect();
            let short = |c: char| expand(&format!("{c}{c}"));
            (short(chars[0])?, short(chars[1])?, short(chars[2])?)
        }
        6 => (
            expand(&digits[0..2])?,
            expand(&digits[2..4])?,
            expand(&digits[4..6])?,
        ),
        _ => return None,
    };
    Some(
        gpui_kit::Rgba {
            r: r as f32 / 255.,
            g: g as f32 / 255.,
            b: b as f32 / 255.,
            a: 1.0,
        }
        .into(),
    )
}

/// Hsla → «#RRGGBB» (для сохранения цвета конкретной аннотации).
pub fn to_hex(color: Hsla) -> String {
    let rgba = gpui_kit::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b)
    )
}

/// Фон выделения стиха/слова — полупрозрачный, читается на обеих темах.
pub fn highlight_bg(name: &str, cx: &App) -> Hsla {
    base(name, cx).opacity(0.30)
}

/// Цвет текста слова (word_color) — плотный.
pub fn word_fg(name: &str, cx: &App) -> Hsla {
    base(name, cx)
}

/// Полоса подчёркивания — плотная.
pub fn underline_fg(name: &str, cx: &App) -> Hsla {
    base(name, cx)
}
