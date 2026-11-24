//! Diacritic-insensitive normalization for Greek (and Hebrew, by stripping
//! combining marks) text. Used for case- and accent-insensitive search across
//! Bible text and dictionary topics.
//!
//! The Greek glyph→base mapping is ported from the Flutter Bible reader at
//! `BibleApp/bible_app/lib/core/utils/greek_utils.dart`. Both NFC (precomposed
//! `ά`) and NFD (`α` + U+0301) inputs are handled: `greek_base` collapses
//! precomposed letters to their bare form, and the combining-mark filter
//! drops any remaining marks.
//!
//! Hebrew niqqud and cantillation are entirely combining marks (Unicode blocks
//! 0x0591..0x05C7) which the same filter strips, so Hebrew search becomes
//! consonants-only without a dedicated mapping table.

/// Lowercased, diacritic-stripped form of `value`. Pure ASCII input is fast-
/// pathed (no allocation if already lowercase ASCII would be ideal — currently
/// returns a fresh `String` for symmetry).
pub fn normalize(value: &str) -> String {
    if value.is_ascii() {
        return value.to_ascii_lowercase();
    }
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if is_combining_mark(ch) {
            continue;
        }
        let base = greek_base(ch).unwrap_or(ch);
        for lower in base.to_lowercase() {
            out.push(lower);
        }
    }
    out
}

pub(crate) fn is_combining_mark(ch: char) -> bool {
    let code = ch as u32;
    (0x0300..=0x036F).contains(&code)
        || (0x0483..=0x0489).contains(&code)
        || (0x0591..=0x05BD).contains(&code)
        || code == 0x05BF
        || (0x05C1..=0x05C2).contains(&code)
        || (0x05C4..=0x05C5).contains(&code)
        || code == 0x05C7
        || (0x1AB0..=0x1AFF).contains(&code)
        || (0x1DC0..=0x1DFF).contains(&code)
        || (0x20D0..=0x20FF).contains(&code)
        || (0xFE20..=0xFE2F).contains(&code)
}

/// Map a Greek letter (with or without diacritics) to its bare base form.
/// Returns `None` for non-Greek characters; the caller keeps them as-is.
fn greek_base(c: char) -> Option<char> {
    match c {
        // alpha
        'ά' | 'ἀ' | 'ἁ' | 'ἂ' | 'ἃ' | 'ἄ' | 'ἅ' | 'ἆ' | 'ἇ' | 'ᾀ' | 'ᾁ' | 'ᾂ' | 'ᾃ' | 'ᾄ' | 'ᾅ'
        | 'ᾆ' | 'ᾇ' | 'ᾰ' | 'ᾱ' | 'ᾲ' | 'ᾳ' | 'ᾴ' | 'ᾶ' | 'ᾷ' | 'Ά' | 'Ἀ' | 'Ἁ' | 'Ἂ' | 'Ἃ'
        | 'Ἄ' | 'Ἅ' | 'Ἆ' | 'Ἇ' | 'ᾈ' | 'ᾉ' | 'ᾊ' | 'ᾋ' | 'ᾌ' | 'ᾍ' | 'ᾎ' | 'ᾏ' | 'Ᾰ' | 'Ᾱ'
        | 'Ὰ' | 'ᾼ' | 'α' | 'Α' | '\u{1f70}' | '\u{1f71}' => Some('α'),
        // epsilon
        'έ' | 'ἐ' | 'ἑ' | 'ἒ' | 'ἓ' | 'ἔ' | 'ἕ' | 'Έ' | 'Ἐ' | 'Ἑ' | 'Ἒ' | 'Ἓ' | 'Ἔ' | 'Ἕ' | 'Ὲ'
        | 'ε' | 'Ε' | '\u{1f72}' | '\u{1f73}' => Some('ε'),
        // eta
        'ή' | 'ἠ' | 'ἡ' | 'ἢ' | 'ἣ' | 'ἤ' | 'ἥ' | 'ἦ' | 'ἧ' | 'ᾐ' | 'ᾑ' | 'ᾒ' | 'ᾓ' | 'ᾔ' | 'ᾕ'
        | 'ᾖ' | 'ᾗ' | 'ῂ' | 'ῃ' | 'ῄ' | 'ῆ' | 'ῇ' | 'Ή' | 'Ἠ' | 'Ἡ' | 'Ἢ' | 'Ἣ' | 'Ἤ' | 'Ἥ'
        | 'Ἦ' | 'Ἧ' | 'ᾘ' | 'ᾙ' | 'ᾚ' | 'ᾛ' | 'ᾜ' | 'ᾝ' | 'ᾞ' | 'ᾟ' | 'Ὴ' | 'ῌ' | 'η' | 'Η'
        | '\u{1f74}' | '\u{1f75}' => Some('η'),
        // iota
        'ί' | 'ἰ' | 'ἱ' | 'ἲ' | 'ἳ' | 'ἴ' | 'ἵ' | 'ἶ' | 'ἷ' | 'ῐ' | 'ῑ' | 'ῒ' | 'ΐ' | 'ῖ' | 'ῗ'
        | 'Ί' | 'Ἰ' | 'Ἱ' | 'Ἲ' | 'Ἳ' | 'Ἴ' | 'Ἵ' | 'Ἶ' | 'Ἷ' | 'Ῐ' | 'Ῑ' | 'Ὶ' | 'ι' | 'Ι'
        | '\u{1f76}' | '\u{1f77}' => Some('ι'),
        // omicron
        'ό' | 'ὀ' | 'ὁ' | 'ὂ' | 'ὃ' | 'ὄ' | 'ὅ' | 'Ό' | 'Ὀ' | 'Ὁ' | 'Ὂ' | 'Ὃ' | 'Ὄ' | 'Ὅ' | 'Ὸ'
        | 'ο' | 'Ο' | '\u{1f78}' | '\u{1f79}' => Some('ο'),
        // upsilon
        'ύ' | 'ὐ' | 'ὑ' | 'ὒ' | 'ὓ' | 'ὔ' | 'ὕ' | 'ὖ' | 'ὗ' | 'ῠ' | 'ῡ' | 'ῢ' | 'ΰ' | 'ῦ' | 'ῧ'
        | 'Ύ' | 'Ὑ' | 'Ὓ' | 'Ὕ' | 'Ὗ' | 'Ῠ' | 'Ῡ' | 'Ὺ' | 'υ' | 'Υ' | '\u{1f7a}' | '\u{1f7b}' => {
            Some('υ')
        }
        // omega
        'ώ' | 'ὠ' | 'ὡ' | 'ὢ' | 'ὣ' | 'ὤ' | 'ὥ' | 'ὦ' | 'ὧ' | 'ᾠ' | 'ᾡ' | 'ᾢ' | 'ᾣ' | 'ᾤ' | 'ᾥ'
        | 'ᾦ' | 'ᾧ' | 'ῲ' | 'ῳ' | 'ῴ' | 'ῶ' | 'ῷ' | 'Ώ' | 'Ὠ' | 'Ὡ' | 'Ὢ' | 'Ὣ' | 'Ὤ' | 'Ὥ'
        | 'Ὦ' | 'Ὧ' | 'ᾨ' | 'ᾩ' | 'ᾪ' | 'ᾫ' | 'ᾬ' | 'ᾭ' | 'ᾮ' | 'ᾯ' | 'Ὼ' | 'ῼ' | 'ω' | 'Ω'
        | '\u{1f7c}' | '\u{1f7d}' => Some('ω'),
        // rho (incl. with breathing)
        'ῤ' | 'ῥ' | 'Ῥ' | 'ρ' | 'Ρ' => Some('ρ'),
        // sigma — final and capital fold to lowercase sigma
        'σ' | 'ς' | 'Σ' => Some('σ'),
        // remaining capitals / consonants — fold to lowercase base
        'β' | 'Β' => Some('β'),
        'γ' | 'Γ' => Some('γ'),
        'δ' | 'Δ' => Some('δ'),
        'ζ' | 'Ζ' => Some('ζ'),
        'θ' | 'Θ' => Some('θ'),
        'κ' | 'Κ' => Some('κ'),
        'λ' | 'Λ' => Some('λ'),
        'μ' | 'Μ' => Some('μ'),
        'ν' | 'Ν' => Some('ν'),
        'ξ' | 'Ξ' => Some('ξ'),
        'π' | 'Π' => Some('π'),
        'τ' | 'Τ' => Some('τ'),
        'φ' | 'Φ' => Some('φ'),
        'χ' | 'Χ' => Some('χ'),
        'ψ' | 'Ψ' => Some('ψ'),
        _ => None,
    }
}

/// Obsidian-compatible heading slug: lowercase Unicode-alphanumeric runs
/// joined by single dashes. Used by `bible_notes` for `[[Note#Heading]]`
/// resolution and by `bible::dictionary` for `[[dict:…#section]]` anchor
/// matching, so both sides must agree on the spelling.
pub fn slug_for_anchor(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_dash = true;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for low in ch.to_lowercase() {
                out.push(low);
            }
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_precomposed_letters_collapse_to_base() {
        // καλός — precomposed (NFC). Result is "καλοσ": diacritics gone,
        // final-sigma `ς` folds to medial `σ` (matches Flutter behaviour so
        // that "λογος" and "λόγος" search-equivalent regardless of position).
        assert_eq!(normalize("καλός"), "καλοσ");
        assert_eq!(normalize("Λόγος"), "λογοσ");
    }

    #[test]
    fn nfd_decomposed_letters_collapse_via_combining_strip() {
        // alpha + U+0301 (combining acute). Must reduce to plain "α".
        let nfd = "α\u{0301}";
        assert_eq!(normalize(nfd), "α");
    }

    #[test]
    fn final_sigma_folds_to_regular_sigma() {
        assert_eq!(normalize("λόγος"), "λογοσ");
    }

    #[test]
    fn hebrew_niqqud_stripped() {
        // בְּרֵאשִׁית = consonants ב ר א ש י ת with niqqud.
        let nfd = "בְּרֵאשִׁית";
        let normalized = normalize(nfd);
        // Niqqud (combining marks 0x0591–0x05C7) must be gone; the bare
        // consonants survive.
        for ch in normalized.chars() {
            let code = ch as u32;
            assert!(
                !(0x0591..=0x05C7).contains(&code),
                "char {ch:?} (U+{code:04X}) should have been stripped"
            );
        }
    }

    #[test]
    fn ascii_fast_path() {
        assert_eq!(normalize("Word"), "word");
        assert_eq!(normalize("MIXED"), "mixed");
    }

    #[test]
    fn polytonic_breathing_marks_dropped() {
        // Ἀρχῇ → "αρχη"
        assert_eq!(normalize("Ἀρχῇ"), "αρχη");
    }
}
