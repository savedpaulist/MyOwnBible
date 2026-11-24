#!/usr/bin/env python3
"""Конвертер темы Everforest (формат Zed) в формат gpui-component ThemeSet.

Вход:  reference/zed-everforest-main/themes/everforest-regular.json
Выход: crates/bible_ui/assets/themes/everforest.json

Запуск из корня репозитория: python tools/convert_everforest.py
"""

import colorsys
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "BibleOWN" / "reference" / "zed-everforest-main" / "themes" / "everforest-regular.json"
TARGET = ROOT / "crates" / "bible_ui" / "assets" / "themes" / "everforest.json"


def hex_to_rgb(value: str) -> tuple[float, float, float]:
    value = value.lstrip("#")[:6]
    return tuple(int(value[i : i + 2], 16) / 255 for i in (0, 2, 4))


def rgb_to_hex(rgb: tuple[float, float, float]) -> str:
    return "#" + "".join(f"{round(c * 255):02x}" for c in rgb)


def adjust_lightness(value: str, delta: float) -> str:
    """Сдвиг lightness в HSL на delta (-1..1) — hover/active варианты."""
    r, g, b = hex_to_rgb(value)
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    l = min(1.0, max(0.0, l + delta))
    r, g, b = colorsys.hls_to_rgb(h, l, s)
    return rgb_to_hex((r, g, b))


def convert(theme: dict) -> dict:
    style = theme["style"]
    dark = theme["appearance"] == "dark"
    # Hover на тёмной теме светлее, на светлой — темнее.
    step = 0.06 if dark else -0.06

    def g(key: str, default: str | None = None) -> str:
        value = style.get(key, default)
        if value is None:
            raise KeyError(f"{theme['name']}: нет токена {key}")
        return value

    fg = g("editor.foreground")
    editor_bg = g("editor.background")
    panel_bg = g("panel.background")
    element_bg = g("element.background")
    hover = g("element.hover")
    active = g("element.active")
    accent = g("text.accent")
    muted_fg = g("text.muted")
    border = g("border")
    border_variant = g("border.variant")
    border_focused = g("border.focused")
    error = g("error")
    warning = g("conflict")
    info = g("link_text.hover")
    success = g("success")
    # Текст на насыщенной кнопке: на тёмной теме — тёмный фон редактора,
    # на светлой — почти белый фон редактора.
    on_accent = editor_bg if dark else g("editor.background")
    if not dark:
        on_accent = adjust_lightness(editor_bg, 0.02)

    accents = style["accents"]
    base = {
        "base.red": accents[0],
        "base.red.light": adjust_lightness(accents[0], 0.15),
        "base.yellow": accents[2],
        "base.yellow.light": adjust_lightness(accents[2], 0.15),
        "base.green": accents[3],
        "base.green.light": adjust_lightness(accents[3], 0.15),
        "base.cyan": accents[4],
        "base.cyan.light": adjust_lightness(accents[4], 0.15),
        "base.blue": accents[5],
        "base.blue.light": adjust_lightness(accents[5], 0.15),
        "base.magenta": accents[6],
        "base.magenta.light": adjust_lightness(accents[6], 0.15),
    }

    colors = {
        "background": editor_bg,
        "foreground": fg,
        "caret": fg,
        "border": border,
        "ring": border_focused,
        "muted.background": element_bg,
        "muted.foreground": muted_fg,
        "popover.background": g("elevated_surface.background"),
        "popover.foreground": fg,
        "accent.background": hover,
        "accent.foreground": fg,
        "primary.background": accent,
        "primary.foreground": on_accent,
        "primary.hover.background": adjust_lightness(accent, step),
        "primary.active.background": adjust_lightness(accent, step * 2),
        "secondary.background": element_bg,
        "secondary.foreground": fg,
        "secondary.hover.background": hover,
        "secondary.active.background": active,
        "danger.background": error,
        "danger.foreground": on_accent,
        "danger.hover.background": adjust_lightness(error, step),
        "danger.active.background": adjust_lightness(error, step * 2),
        "warning.background": warning,
        "warning.foreground": on_accent,
        "warning.hover.background": adjust_lightness(warning, step),
        "warning.active.background": adjust_lightness(warning, step * 2),
        "info.background": info,
        "info.foreground": on_accent,
        "info.hover.background": adjust_lightness(info, step),
        "info.active.background": adjust_lightness(info, step * 2),
        "success.background": success,
        "success.foreground": on_accent,
        "success.hover.background": adjust_lightness(success, step),
        "success.active.background": adjust_lightness(success, step * 2),
        "link": info,
        "link.hover": adjust_lightness(info, step),
        "link.active": adjust_lightness(info, step * 2),
        "selection.background": g("element.selection_background"),
        "list.background": panel_bg,
        "list.hover.background": hover,
        "list.active.background": active,
        "list.active.border": border_focused,
        "list.even.background": element_bg,
        "list.head.background": panel_bg,
        "list.highlight.foreground": accent,
        "sidebar.background": panel_bg,
        "sidebar.foreground": fg,
        "sidebar.border": border_variant,
        "sidebar.accent.background": hover,
        "sidebar.accent.foreground": fg,
        "sidebar.primary.background": accent,
        "sidebar.primary.foreground": on_accent,
        "tab.background": g("tab.inactive_background"),
        "tab.foreground": muted_fg,
        "tab.active.background": g("tab.active_background"),
        "tab.active.foreground": fg,
        "tab_bar.background": g("tab_bar.background"),
        "tab_bar.segmented.background": element_bg,
        "title_bar.background": g("title_bar.background"),
        "title_bar.border": border_variant,
        "status_bar.background": g("status_bar.background"),
        "status_bar.border": border_variant,
        "scrollbar.background": "#00000000",
        "scrollbar.thumb.background": g("scrollbar.thumb.background"),
        "scrollbar.thumb.hover.background": g("scrollbar.thumb.hover_background"),
        "input.border": border,
        "accordion.background": panel_bg,
        "group_box.background": element_bg,
        "group_box.foreground": fg,
        "group_box.title.foreground": muted_fg,
        "description_list.label.foreground": muted_fg,
        "skeleton.background": element_bg,
        "drag.border": border_focused,
        "drop_target.background": g("drop_target.background"),
        "overlay": fg + "80",
        "window.border": border_variant,
        "switch.background": active,
        "switch.thumb.background": fg,
        "slider.background": element_bg,
        "slider.thumb.background": accent,
        "progress.bar.background": accent,
        "table.background": editor_bg,
        "table.head.background": panel_bg,
        "table.head.foreground": muted_fg,
        "table.hover.background": hover,
        "table.active.background": active,
        "table.active.border": border_focused,
        "table.even.background": panel_bg,
        "table.row.border": border_variant,
        "table.foot.background": panel_bg,
        "table.foot.foreground": muted_fg,
        "chart.1": accents[3],
        "chart.2": accents[5],
        "chart.3": accents[2],
        "chart.4": accents[0],
        "chart.5": accents[6],
        "chart.bullish": success,
        "chart.bearish": error,
        "chart.grid": border_variant,
        **base,
    }

    name = theme["name"].replace(" (regular)", "")
    return {
        "name": name,
        "mode": "dark" if dark else "light",
        "is_default": name in ("Everforest Dark Hard", "Everforest Light Hard"),
        # Плоский вид без теней — как в Zed.
        "shadow": False,
        "colors": colors,
    }


def main() -> None:
    data = json.loads(SOURCE.read_text(encoding="utf-8"))
    themes = [convert(theme) for theme in data["themes"]]
    output = {
        "name": "Everforest",
        "author": data.get("author", ""),
        "url": "https://github.com/albertsko/zed-everforest",
        "themes": themes,
    }
    TARGET.parent.mkdir(parents=True, exist_ok=True)
    TARGET.write_text(
        json.dumps(output, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"written {TARGET} ({len(themes)} themes)")


if __name__ == "__main__":
    main()
