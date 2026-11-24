# Zed themes in BibleOWN

BibleOWN reads Zed theme-family JSON files on startup and converts the UI palette
into the colors used by gpui-kit. Restart BibleOWN after adding or replacing a
file.

## BibleOWN theme folder

- Windows: `%APPDATA%\\BibleOWN\\themes`
- macOS: `~/Library/Application Support/BibleOWN/themes`
- Linux: `$XDG_DATA_HOME/BibleOWN/themes`, or `~/.local/share/BibleOWN/themes`

The application creates this directory automatically. Put one or more `.json`
files in it. Zed themes placed in the normal Zed theme folder and installed Zed
extension theme files are also discovered automatically.

## Zed theme sources scanned

- Windows: `%APPDATA%\\Zed\\themes` and `%LOCALAPPDATA%\\Zed\\extensions\\installed`
- macOS: `~/Library/Application Support/Zed/themes` and
  `~/Library/Application Support/Zed/extensions/installed`
- Linux: `$XDG_CONFIG_HOME/zed/themes` (or `~/.config/zed/themes`) and
  `$XDG_DATA_HOME/zed/extensions/installed` (or `~/.local/share/zed/extensions/installed`)

The importer accepts Zed Theme Family JSON (`name`, `author`, `themes`, with
`appearance` and `style` per theme). It maps common surface, text, border,
selection, status, accent, and ANSI colors to the BibleOWN/gpui-kit palette.
Zed-specific syntax highlighting scopes and terminal-only settings are not
ported because BibleOWN does not contain a code editor or terminal.
