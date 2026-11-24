//! Script-aware direction helpers for segmented Bible text.
//!
//! Text remains in logical Unicode order. This helper only decides whether the
//! token container should flow RTL; glyph order is handled by the Windows
//! DirectWrite renderer patch shipped in `tools/apply_gpui_rtl_fix.ps1`.

/// Returns true only when the paragraph has Hebrew letters and no strong
/// letters from another script. Neutrals (spaces, punctuation, numerals and
/// Hebrew niqqud/cantillation marks) do not change the result.
pub fn is_hebrew_paragraph(text: &str) -> bool {
    let mut has_hebrew_letter = false;
    let mut has_other_letter = false;

    for ch in text.chars() {
        if is_hebrew_letter(ch) {
            has_hebrew_letter = true;
        } else if ch.is_alphabetic() && !matches!(ch as u32, 0x0591..=0x05C7) {
            has_other_letter = true;
        }
    }

    has_hebrew_letter && !has_other_letter
}

/// Hebrew letters and presentation forms; deliberately excludes combining
/// marks, punctuation and digits, which are handled as neutral characters.
fn is_hebrew_letter(ch: char) -> bool {
    matches!(
        ch as u32,
        0x05D0..=0x05EA | 0x05EF..=0x05F2 | 0xFB1D | 0xFB1F..=0xFB4F
    )
}

#[cfg(test)]
mod tests {
    use super::is_hebrew_paragraph;

    #[test]
    fn recognizes_hebrew_with_niqqud_and_cantillation() {
        assert!(is_hebrew_paragraph("בְּרֵאשִׁ֖ית בָּרָ֣א אֱלֹהִ֑ים"));
        assert!(is_hebrew_paragraph("וַיֹּ֣אמֶר 123, אֱלֹהִ֔ים"));
    }

    #[test]
    fn leaves_greek_russian_and_english_ltr() {
        assert!(!is_hebrew_paragraph("Ἐν ἀρχῇ ἦν ὁ λόγος"));
        assert!(!is_hebrew_paragraph("В начале было Слово"));
        assert!(!is_hebrew_paragraph("In the beginning was the Word"));
    }

    #[test]
    fn mixed_scripts_are_not_forced_into_hebrew_flow() {
        assert!(!is_hebrew_paragraph("בְּרֵאשִׁית beginning"));
        assert!(!is_hebrew_paragraph("Ἐν ἀρχῇ בְּרֵאשִׁית"));
    }
}
