//! Theme-aware colours for Greek and Hebrew morphology.
//!
//! MyBible uses two code families: uppercase, dash-separated codes
//! (`V-AAI-3S`, `CONJ`) for Greek and lower-case, dot-separated codes
//! (`v.qal.pf.3ms`, `prep`) for Hebrew. Both are reduced to one visual
//! parts-of-speech palette. Every foreground is adjusted to WCAG text
//! contrast against the current reader background, including user overrides.

use std::collections::BTreeMap;

use gpui_kit::{Hsla, Rgba, hsla};

const MIN_TEXT_CONTRAST: f32 = 4.5;

pub(crate) fn color(
    morph: &str,
    overrides: &BTreeMap<String, String>,
    background: Hsla,
) -> Option<Hsla> {
    let category = category(morph)?;
    let seed = overrides
        .get(category)
        .and_then(|hex| crate::annotations::parse_hex(hex))
        .unwrap_or_else(|| category_seed(category));
    // A fixed, high-chroma palette looks fluorescent on a charcoal/grey
    // surface.  First fit the colour's chroma and perceived lightness to the
    // actual surface, then use the WCAG pass only as a safety net.  Overrides
    // deliberately follow the same rule: they remain recognisable but never
    // become a visual hole in a calm theme.
    Some(ensure_contrast(
        harmonise_with_background(seed, background),
        background,
        MIN_TEXT_CONTRAST,
    ))
}

/// Common visual grammar for the Greek and Hebrew code tables.
fn category(morph: &str) -> Option<&'static str> {
    let head = morph.trim().split(['-', '.']).next()?.to_ascii_lowercase();
    match head.as_str() {
        "prep" => Some("preposition"),
        "conj" => Some("conjunction"),
        "adv" | "advb" => Some("adverb"),
        "inj" | "intj" => Some("interjection"),
        "v" => Some("verb"),
        "n" | "np" => Some("noun"),
        "a" => Some("adjective"),
        // Greek pronoun families: P/R/C/I/X/F/D/S/K/Q/RI.
        // Hebrew pronoun families: prps/prde/prin/inrg.
        "p" | "r" | "c" | "i" | "x" | "f" | "d" | "s" | "k" | "q" | "ri" | "prps" | "prde"
        | "prin" | "inrg" => Some("pronoun"),
        // Articles, particles, Hebrew negation and ketiv/qere marker.
        "t" | "prt" | "cond" | "art" | "nega" | "kr" => Some("particle"),
        _ => None,
    }
}

/// Stable hues keep categories recognisable; only required lightness changes.
fn category_seed(category: &str) -> Hsla {
    match category {
        "verb" => hsla(0.01, 0.78, 0.50, 1.0),
        "noun" => hsla(0.61, 0.76, 0.52, 1.0),
        "adjective" => hsla(0.34, 0.68, 0.42, 1.0),
        "preposition" => hsla(0.52, 0.72, 0.43, 1.0),
        "conjunction" => hsla(0.08, 0.86, 0.48, 1.0),
        "adverb" => hsla(0.76, 0.68, 0.55, 1.0),
        "interjection" => hsla(0.94, 0.76, 0.52, 1.0),
        "particle" => hsla(0.13, 0.86, 0.44, 1.0),
        "pronoun" => hsla(0.47, 0.67, 0.40, 1.0),
        _ => hsla(0.0, 0.0, 0.5, 1.0),
    }
}

/// Fits an accent to its canvas before contrast correction.
///
/// `s * (1 - |2l - 1|)` is HSL chroma.  It lets neutral grey backgrounds
/// deliberately reduce categorical saturation, whereas a coloured theme can
/// retain a little more of its own character.  The bounded lightness target
/// keeps dark themes soft rather than neon while preserving enough headroom
/// for the contrast solver below.
fn harmonise_with_background(mut foreground: Hsla, background: Hsla) -> Hsla {
    let bg_chroma = background.s * (1.0 - (2.0 * background.l - 1.0).abs());
    let dark_surface = background.l < 0.5;
    let saturation_cap = if dark_surface {
        // 42–53% is deliberately quieter than the former 67–86% seeds.
        0.42 + bg_chroma * 0.11
    } else {
        0.50 + bg_chroma * 0.12
    };
    foreground.s = foreground.s.min(saturation_cap);

    // Move bright colours on dark themes toward a shared editorial ink value;
    // on light themes move them toward a calm, darker ink value. The original
    // lightness still contributes, so categories do not collapse together.
    let anchor = if dark_surface { 0.66 } else { 0.37 };
    foreground.l = foreground.l * 0.30 + anchor * 0.70;
    foreground.a = 1.0;
    foreground
}

fn ensure_contrast(mut foreground: Hsla, background: Hsla, target: f32) -> Hsla {
    foreground.a = 1.0;
    if contrast_ratio(foreground, background) >= target {
        return foreground;
    }

    let black_ratio = contrast_ratio(Hsla::black(), background);
    let white_ratio = contrast_ratio(Hsla::white(), background);
    let endpoint = if black_ratio >= white_ratio { 0.0 } else { 1.0 };

    // Binary-search the first passing lightness: the smallest visible change
    // from the requested colour, while retaining category hue and saturation.
    let original = foreground.l;
    let (mut failing, mut passing) = (0.0, 1.0);
    for _ in 0..12 {
        let amount = (failing + passing) / 2.0;
        foreground.l = original + (endpoint - original) * amount;
        if contrast_ratio(foreground, background) >= target {
            passing = amount;
        } else {
            failing = amount;
        }
    }
    foreground.l = original + (endpoint - original) * passing;
    foreground
}

fn contrast_ratio(first: Hsla, second: Hsla) -> f32 {
    let first = relative_luminance(composite_over_opaque(first, second));
    let second = relative_luminance(opaque_background(second));
    let (lighter, darker) = if first >= second {
        (first, second)
    } else {
        (second, first)
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn opaque_background(background: Hsla) -> Rgba {
    let rgb = Rgba::from(background);
    Rgba {
        r: 1.0 * (1.0 - rgb.a) + rgb.r * rgb.a,
        g: 1.0 * (1.0 - rgb.a) + rgb.g * rgb.a,
        b: 1.0 * (1.0 - rgb.a) + rgb.b * rgb.a,
        a: 1.0,
    }
}

fn composite_over_opaque(foreground: Hsla, background: Hsla) -> Rgba {
    let fg = Rgba::from(foreground);
    let bg = opaque_background(background);
    Rgba {
        r: bg.r * (1.0 - fg.a) + fg.r * fg.a,
        g: bg.g * (1.0 - fg.a) + fg.g * fg.a,
        b: bg.b * (1.0 - fg.a) + fg.b * fg.a,
        a: 1.0,
    }
}

fn relative_luminance(rgb: Rgba) -> f32 {
    let channel = |value: f32| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_greek_and_hebrew_grammatical_tables() {
        assert_eq!(category("CONJ"), Some("conjunction"));
        assert_eq!(category("V-AAI-3S"), Some("verb"));
        assert_eq!(category("T-NSM"), Some("particle"));
        assert_eq!(category("COND"), Some("particle"));
        assert_eq!(category("Q-NSM"), Some("pronoun"));
        assert_eq!(category("v.qal.wa.3ms"), Some("verb"));
        assert_eq!(category("np.ms.a"), Some("noun"));
        assert_eq!(category("prps.3ms"), Some("pronoun"));
        assert_eq!(category("nega"), Some("particle"));
        assert_eq!(category("kr"), Some("particle"));
    }

    #[test]
    fn every_category_is_readable_on_light_and_dark_themes() {
        let overrides = BTreeMap::new();
        let backgrounds = [hsla(0.0, 0.0, 0.98, 1.0), hsla(0.60, 0.18, 0.08, 1.0)];
        for background in backgrounds {
            for morph in [
                "V-AAI-3S",
                "N-NSM",
                "A-NSM",
                "PREP",
                "CONJ",
                "ADV",
                "INJ",
                "PRT",
                "P-GS",
                "v.qal.pf.3ms",
                "np.ms.a",
                "art",
                "prde.ms",
            ] {
                let foreground = color(morph, &overrides, background).unwrap();
                assert!(
                    contrast_ratio(foreground, background) >= MIN_TEXT_CONTRAST,
                    "{morph} has insufficient contrast on {background:?}"
                );
            }
        }
    }

    #[test]
    fn unsafe_user_override_is_corrected_for_current_background() {
        let mut overrides = BTreeMap::new();
        overrides.insert("conjunction".to_string(), "#FFFFFF".to_string());
        let white = hsla(0.0, 0.0, 1.0, 1.0);
        let corrected = color("CONJ", &overrides, white).unwrap();
        assert!(contrast_ratio(corrected, white) >= MIN_TEXT_CONTRAST);
        assert!(corrected.l < 0.5);
    }

    #[test]
    fn grey_dark_surfaces_tame_morphology_chroma_before_contrast_correction() {
        let neutral_dark = hsla(0.0, 0.0, 0.13, 1.0);
        let coloured_dark = hsla(0.58, 0.35, 0.13, 1.0);
        let red = category_seed("verb");
        let neutral = harmonise_with_background(red, neutral_dark);
        let coloured = harmonise_with_background(red, coloured_dark);
        assert!(neutral.s <= 0.43);
        assert!(neutral.s < coloured.s);
        assert!(
            contrast_ratio(
                ensure_contrast(neutral, neutral_dark, MIN_TEXT_CONTRAST),
                neutral_dark
            ) >= MIN_TEXT_CONTRAST
        );
    }
}
