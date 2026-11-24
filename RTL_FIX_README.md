# Hebrew / RTL rendering fix for BibleOWN on Windows

## What was wrong

The earlier UI-only solution used `flex_row_reverse()` to reorder the individual word widgets in Hebrew-only verses. That can put words in right-to-left order, but it cannot reverse the glyph placement inside a word. Reversing the source string or the full row would also corrupt Greek, Russian, English, punctuation, and mixed-script text.

## Correct separation of responsibilities

BibleOWN keeps the original Unicode text in **logical order**. Its small script check only reverses the order of separate word widgets for a paragraph that contains Hebrew letters and no strong letters from another alphabet. Greek, Russian, English, and mixed-script paragraphs are not reversed by the app.

The actual letter order is handled by the Windows DirectWrite renderer, using DirectWrite's per-run bidirectional level. The renderer patch also maps UTF-16 offsets back to UTF-8 indices without assuming that glyph indices ascend. GPUI's `LineLayout::split_at` is updated to partition visually ordered RTL glyphs safely. This matters for selection, wrapping, and shaped-line splitting; changing only the positions of the letters is not enough.

## Upstream implementation

The Windows and GPUI-core changes are ported from the two commits in Zed PR [#67281](https://github.com/zed-industries/zed/pull/67281):

- `0027a56b212c9a89b2ab3e8ae94d019e20460d1d` — DirectWrite glyph-run visual order, glyph-to-text index mapping, and RTL shaping tests.
- `3aad1f873332d67c9360b33ec47b2409a165a515` — safe line splitting for visually ordered RTL glyphs and width accounting.

The PR is open upstream as of 23 November 2026. This is therefore an explicit, version-checked local port, not a claim that the fix is already part of the published GPUI release.

## Apply on Windows

The ZIP supplied for this project does not include the root `Cargo.toml` and `Cargo.lock`; the script must be run from the full local BibleOWN workspace root, where both files exist.

1. In PowerShell, go to the project root (the folder containing `Cargo.toml` and `Cargo.lock`).
2. Run `cargo fetch` so the pinned GPUI PRE sources exist in the Cargo registry.
3. Ensure Git for Windows is installed and available as `git` in PATH.
4. Run:

   ```powershell
   powershell -ExecutionPolicy Bypass -File .\tools\apply_gpui_rtl_fix.ps1
   ```

The script downloads the two immutable upstream diffs, applies them first to temporary copies, and checks both patches before changing the project. If the source does not match the expected GPUI PRE 0.3.12 snapshot, it stops without changing the project files. On success, it vendors only `gpui-pre` and `gpui-pre-windows`, adds them to `[patch.crates-io]`, and creates a backup of the root `Cargo.toml`.

Then run:

```powershell
cargo check
cargo test -p gpui-pre-windows rtl_runs_are_laid_out_in_visual_order
cargo test -p gpui-pre test_split_at_descending_glyph_indices
```

Cargo will update `Cargo.lock` to use the local path overrides. Keep the generated `vendor/gpui-pre` and `vendor/gpui-pre-windows` folders with the project; do not delete them after applying the patch.

## Manual visual checks

Check these in the Bible reader and article previews:

- Hebrew with niqqud/cantillation: `בְּרֵאשִׁ֖ית בָּרָ֣א אֱלֹהִ֑ים`
- Hebrew plain: `שלום עולם`
- Mixed Hebrew/English: `Hello שלום world עולם 123`
- Greek: `Ἐν ἀρχῇ ἦν ὁ λόγος`
- Russian: `В начале было Слово`
- English: `In the beginning was the Word`

Expected: Hebrew letters are shaped and displayed right-to-left; word order in Hebrew-only Bible verses remains right-to-left; Greek, Russian, and English remain left-to-right. Mixed text inside one shaped line is handled at the glyph-run level by DirectWrite. In BibleOWN's tokenized word-widget view, only Hebrew-only paragraphs get RTL widget ordering; mixed-script paragraphs are deliberately not forced into an incorrect whole-row reversal.

## Rollback

1. Restore the root manifest from `Cargo.toml.bibleown-rtl.bak`.
2. Remove `vendor/gpui-pre` and `vendor/gpui-pre-windows`.
3. Run `cargo check` to let Cargo return to the registry versions.

The script does not modify `%USERPROFILE%\.cargo\registry`.


### Installer update (PowerShell 5.1)

The installer now checks for existing `gpui-pre` entries only inside the `[patch.crates-io]` table. Normal declarations under `[dependencies]` or `[workspace.dependencies]` no longer trigger a false-positive. An actual existing patch entry is still preserved and causes a clear stop rather than being overwritten.
