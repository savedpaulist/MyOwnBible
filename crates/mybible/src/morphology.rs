//! Расшифровка морфологического кода вида `V-AAI-3S` в человекочитаемый
//! текст (architecture.md §6.4).
//!
//! Перенесено из форка (`bible/src/morphology.rs`) без изменений логики.
//! Поддерживаются две схемы таблицы `morphology_indications`:
//!
//! 1. Схема Strong's-словарей: `morphology_indications(indication, meaning)`,
//!    у одного indication может быть несколько значений — выбирается то,
//!    чьи теги лучше подходят базовой части речи.
//! 2. Схема SECE: добавлена колонка `applicable_to` — базовый запрос идёт с
//!    `applicable_to=''`, сегменты хвоста — с `applicable_to=<base>`, при
//!    пустом результате — без ограничения.

use anyhow::{Context as _, Result};
use sqlez::connection::Connection;

use crate::db::ReadOnlyDb;

pub fn resolve(db: &ReadOnlyDb, code: &str, uses_applicable_to: bool) -> Result<String> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let resolved = db.with(|conn| {
        if uses_applicable_to {
            resolve_sece(conn, trimmed)
        } else {
            resolve_strongs(conn, trimmed)
        }
    })?;
    // В значениях MyBible встречаются типографские пробелы (U+200A и др.)
    // внутри сокращений вроде «наст. вр.» — схлопываем любые пробельные
    // последовательности в обычный ASCII-пробел.
    Ok(resolved.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn resolve_strongs(conn: &Connection, code: &str) -> Result<String> {
    if let Some(meaning) = lookup_strongs(conn, code, base_code(code))? {
        return Ok(meaning);
    }

    let mut results = Vec::new();
    let raw_parts: Vec<&str> = code.split('-').collect();
    let base = base_code(code);

    for (seg_idx, seg) in raw_parts.iter().enumerate() {
        if seg.is_empty() {
            continue;
        }
        let indication = if seg_idx == 0 {
            (*seg).to_string()
        } else {
            format!("-{seg}")
        };

        if let Some(meaning) = lookup_strongs(conn, &indication, base)? {
            results.push(meaning);
            continue;
        }

        if seg.chars().count() > 1 {
            let mut found = false;
            for len in 1..seg.len() {
                if !seg.is_char_boundary(len) {
                    continue;
                }
                let prefix = &seg[..len];
                let sub = if seg_idx == 0 {
                    prefix.to_string()
                } else {
                    format!("-{prefix}")
                };
                if let Some(meaning) = lookup_strongs(conn, &sub, base)? {
                    results.push(meaning);
                    found = true;
                    break;
                }
            }
            if !found && seg.len() >= 2 {
                let suffix_start = seg.len() - 2;
                if seg.is_char_boundary(suffix_start) {
                    let suf = format!("-{}", &seg[suffix_start..]);
                    if let Some(meaning) = lookup_strongs(conn, &suf, base)? {
                        results.push(meaning);
                    }
                }
            }
        }
    }

    if results.is_empty() {
        Ok(code.to_string())
    } else {
        Ok(results.join(", "))
    }
}

fn lookup_strongs(conn: &Connection, indication: &str, base: &str) -> Result<Option<String>> {
    let mut select = conn
        .select_bound::<&str, String>(
            "SELECT meaning FROM morphology_indications WHERE indication = ?",
        )
        .context("preparing strong meaning lookup")?;
    let meanings = select(indication).context("running strong meaning lookup")?;
    let meanings: Vec<String> = meanings
        .into_iter()
        .filter(|m| !m.trim().is_empty())
        .collect();
    Ok(select_meaning_for_base(&meanings, base))
}

fn resolve_sece(conn: &Connection, code: &str) -> Result<String> {
    if let Some(full) = lookup_sece_first(conn, code, None)? {
        return Ok(full);
    }
    let parts: Vec<&str> = code.split('-').collect();
    if parts.is_empty() {
        return Ok(String::new());
    }

    let mut results = Vec::new();
    let base = parts[0];
    if !base.is_empty() {
        if let Some(meaning) = lookup_sece_first(conn, base, Some(""))? {
            results.push(meaning);
        } else if let Some(meaning) = lookup_sece_first(conn, base, None)? {
            results.push(meaning);
        }
    }

    for seg in parts.iter().skip(1) {
        if seg.is_empty() {
            continue;
        }
        let indication = format!("-{seg}");
        if let Some(meaning) = lookup_sece_first(conn, &indication, Some(base))? {
            results.push(meaning);
            continue;
        }
        if let Some(meaning) = lookup_sece_first(conn, &indication, None)? {
            results.push(meaning);
        }
    }

    Ok(results.join(", "))
}

fn lookup_sece_first(
    conn: &Connection,
    indication: &str,
    applicable_to: Option<&str>,
) -> Result<Option<String>> {
    match applicable_to {
        Some(value) => {
            let mut select = conn
                .select_bound::<(&str, &str), String>(
                    "SELECT meaning FROM morphology_indications \
                     WHERE indication = ? AND applicable_to = ? LIMIT 1",
                )
                .context("preparing SECE meaning lookup with constraint")?;
            let rows = select((indication, value))
                .context("running SECE meaning lookup with constraint")?;
            Ok(rows.into_iter().find(|m| !m.trim().is_empty()))
        }
        None => {
            let mut select = conn
                .select_bound::<&str, String>(
                    "SELECT meaning FROM morphology_indications WHERE indication = ? LIMIT 1",
                )
                .context("preparing SECE meaning lookup")?;
            let rows = select(indication).context("running SECE meaning lookup")?;
            Ok(rows.into_iter().find(|m| !m.trim().is_empty()))
        }
    }
}

fn base_code(code: &str) -> &str {
    code.split('-').next().unwrap_or("")
}

fn select_meaning_for_base(meanings: &[String], base_code: &str) -> Option<String> {
    if meanings.is_empty() {
        return None;
    }
    if meanings.len() == 1 {
        return Some(meanings[0].clone());
    }
    let normalised = base_code.trim().to_uppercase();
    let prefer_verb = normalised == "V";
    let prefer_nominal = matches!(
        normalised.as_str(),
        "A" | "N" | "T" | "P" | "R" | "C" | "D" | "I" | "X" | "F"
    );
    if prefer_verb {
        if let Some(meaning) = meanings.iter().find(|m| looks_like_verb(m)) {
            return Some(meaning.clone());
        }
    }
    if prefer_nominal {
        if let Some(meaning) = meanings.iter().find(|m| looks_like_nominal(m)) {
            return Some(meaning.clone());
        }
    }
    meanings.first().cloned()
}

const VERB_KEYWORDS: &[&str] = &[
    "аор",
    "буд",
    "наст",
    "перф",
    "плюск",
    "импер",
    "индик",
    "сослаг",
    "опт",
    "инфин",
    "прич",
    "страд",
    "действ",
    "сред",
    "депон",
    "pass",
    "aor",
    "fut",
    "perf",
    "pluperf",
    "imp",
    "ind",
    "subj",
    "opt",
    "infin",
    "part",
];

const NOMINAL_KEYWORDS: &[&str] = &[
    "падеж",
    "п.",
    "род",
    "р.",
    "nom",
    "gen",
    "dat",
    "accus",
    "voc",
    "masc",
    "fem",
    "neut",
];

fn looks_like_verb(meaning: &str) -> bool {
    let lower = meaning.to_lowercase();
    VERB_KEYWORDS.iter().any(|k| lower.contains(k))
}

fn looks_like_nominal(meaning: &str) -> bool {
    let lower = meaning.to_lowercase();
    NOMINAL_KEYWORDS.iter().any(|k| lower.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlez::connection::Connection;
    use std::sync::Arc;

    use crate::db::ReadOnlyDb;

    fn fixture_db_strongs() -> Arc<ReadOnlyDb> {
        let conn = Connection::open_memory(None);
        conn.exec("CREATE TABLE morphology_indications (indication TEXT, meaning TEXT)")
            .unwrap()()
        .unwrap();
        {
            let mut insert = conn
                .exec_bound::<(&str, &str)>(
                    "INSERT INTO morphology_indications (indication, meaning) VALUES (?, ?)",
                )
                .unwrap();
            let rows: &[(&str, &str)] = &[
                ("V", "Verb"),
                ("-AAI", "Aorist Active Indicative"),
                ("-3S", "3rd Person Singular"),
                ("N", "Noun"),
                ("-DSF", "Dative Singular Feminine"),
                ("PREP", "Preposition"),
            ];
            for row in rows {
                insert(*row).unwrap();
            }
        }
        Arc::new(ReadOnlyDb::from_connection_for_tests(conn))
    }

    fn fixture_db_sece() -> Arc<ReadOnlyDb> {
        let conn = Connection::open_memory(None);
        conn.exec(
            "CREATE TABLE morphology_indications \
             (indication TEXT, applicable_to TEXT, meaning TEXT)",
        )
        .unwrap()()
        .unwrap();
        {
            let mut insert = conn
                .exec_bound::<(&str, &str, &str)>(
                    "INSERT INTO morphology_indications \
                     (indication, applicable_to, meaning) VALUES (?, ?, ?)",
                )
                .unwrap();
            let rows: &[(&str, &str, &str)] = &[
                ("V", "", "Verb"),
                ("-AAI", "V", "Aorist Active Indicative"),
                ("-3S", "V", "3rd Person Singular"),
                ("N", "", "Noun"),
                ("-DSF", "N", "Dative Singular Feminine"),
                ("PREP", "", "Preposition"),
            ];
            for row in rows {
                insert(*row).unwrap();
            }
        }
        Arc::new(ReadOnlyDb::from_connection_for_tests(conn))
    }

    #[test]
    fn strongs_resolves_full_chain() {
        let db = fixture_db_strongs();
        let result = resolve(&db, "V-AAI-3S", false).unwrap();
        assert_eq!(
            result,
            "Verb, Aorist Active Indicative, 3rd Person Singular"
        );
    }

    #[test]
    fn strongs_resolves_noun() {
        let db = fixture_db_strongs();
        let result = resolve(&db, "N-DSF", false).unwrap();
        assert_eq!(result, "Noun, Dative Singular Feminine");
    }

    #[test]
    fn strongs_returns_raw_for_unknown_code() {
        let db = fixture_db_strongs();
        let result = resolve(&db, "Z-XYZ", false).unwrap();
        assert_eq!(result, "Z-XYZ");
    }

    #[test]
    fn strongs_resolves_simple_prep() {
        let db = fixture_db_strongs();
        let result = resolve(&db, "PREP", false).unwrap();
        assert_eq!(result, "Preposition");
    }

    #[test]
    fn sece_resolves_with_applicable_to() {
        let db = fixture_db_sece();
        let result = resolve(&db, "V-AAI-3S", true).unwrap();
        assert_eq!(
            result,
            "Verb, Aorist Active Indicative, 3rd Person Singular"
        );
    }

    #[test]
    fn sece_resolves_noun_chain() {
        let db = fixture_db_sece();
        let result = resolve(&db, "N-DSF", true).unwrap();
        assert_eq!(result, "Noun, Dative Singular Feminine");
    }

    #[test]
    fn select_meaning_prefers_verb_for_v_base() {
        let result = select_meaning_for_base(
            &[
                "Pronoun (genitive case)".to_string(),
                "Aorist Active".to_string(),
            ],
            "V",
        );
        assert_eq!(result.as_deref(), Some("Aorist Active"));
    }

    #[test]
    fn select_meaning_prefers_nominal_for_n_base() {
        let result = select_meaning_for_base(
            &[
                "subjunctive form".to_string(),
                "Genitive Singular".to_string(),
            ],
            "N",
        );
        assert_eq!(result.as_deref(), Some("Genitive Singular"));
    }

    #[test]
    fn select_meaning_with_single_entry_returns_it() {
        let result = select_meaning_for_base(&["Only".to_string()], "X");
        assert_eq!(result.as_deref(), Some("Only"));
    }
}
