use super::word_key;
use crate::{Result, Store};
use rusqlite::{Connection, ToSql};
use serde::Serialize;
use std::collections::BTreeMap;
use unicode_normalization::char::is_combining_mark;
use unicode_segmentation::UnicodeSegmentation;

const LATIN: &str = "abcdefghijklmnopqrstuvwxyz'";
const FRENCH: &str = "àâäæçéèêëîïôöœùûüÿ";
const UKRAINIAN: &str = "абвгґдеєжзиіїйклмнопрстуфхцчшщьюя'";
const CONFUSIONS: &[(&str, &str)] = &[
    ("0", "o"),
    ("0", "о"),
    ("1", "l"),
    ("1", "i"),
    ("1", "і"),
    ("5", "s"),
    ("8", "b"),
    ("6", "б"),
    ("3", "з"),
    ("rn", "m"),
    ("cl", "d"),
    ("vv", "w"),
    ("l", "і"),
    ("a", "а"),
    ("c", "с"),
    ("e", "е"),
    ("i", "і"),
    ("o", "о"),
    ("p", "р"),
    ("x", "х"),
    ("y", "у"),
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrSuggestion {
    start_utf16: usize,
    length_utf16: usize,
    source: String,
    replacement: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpellingSuggestion {
    replacement: String,
    observations: u32,
    previous_observations: u32,
    next_observations: u32,
    high_confidence: bool,
}

pub(super) fn suggest_spelling(
    db: &Connection,
    language: &str,
    word: &str,
    key: &str,
    left: Option<i64>,
    right: Option<i64>,
) -> Result<Vec<SpellingSuggestion>> {
    if !(3..=32).contains(&key.chars().count())
        || !key.chars().any(char::is_alphabetic)
        || !key.chars().all(|letter| {
            letter.is_alphabetic()
                || letter.is_ascii_digit()
                || letter == '\''
                || is_combining_mark(letter)
        })
    {
        return Ok(Vec::new());
    }
    let candidates = candidate_edits(key, &alphabet(language));
    let candidates = serde_json::to_string(&candidates.into_keys().collect::<Vec<_>>())
        .map_err(|error| error.to_string())?;
    let context_columns = if left.is_some() || right.is_some() {
        "coalesce((SELECT sum(observations) FROM neighbors WHERE term=f.term AND side=0 AND neighbor=?2),0),
         coalesce((SELECT sum(observations) FROM neighbors WHERE term=f.term AND side=1 AND neighbor=?3),0)"
    } else {
        "0,0"
    };
    let mut query = db.prepare(&format!(
        "WITH candidates(term, observations, previous, next) AS (
             SELECT f.term, sum(f.observations), {context_columns}
             FROM json_each(?1) AS c JOIN forms f ON f.term=c.value GROUP BY f.term
         )
         SELECT term, observations, previous, next FROM candidates
         ORDER BY (previous > 0) + (next > 0) DESC, previous + next DESC, observations DESC, term LIMIT 3"
    )).map_err(|error| error.to_string())?;
    let values: &[&dyn ToSql] = &[&candidates, &left, &right];
    let rows = query
        .query_map(&values[..query.parameter_count()], |row| {
            Ok(SpellingSuggestion {
                replacement: preserve_case(word, &row.get::<_, String>(0)?),
                observations: row.get(1)?,
                previous_observations: row.get(2)?,
                next_observations: row.get(3)?,
                high_confidence: false,
            })
        })
        .map_err(|error| error.to_string())?;
    let mut suggestions = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    let supports_both_sides = |candidate: &SpellingSuggestion| {
        candidate.previous_observations > 0 && candidate.next_observations > 0
    };
    // A unique candidate must have corpus support against both adjacent words.
    if let Some((first, alternatives)) = suggestions.split_first_mut() {
        first.high_confidence =
            supports_both_sides(first) && !alternatives.iter().any(supports_both_sides);
    }
    Ok(suggestions)
}

impl Store {
    pub fn suggest_ocr(&self, language: &str, text: &str) -> Result<Option<Vec<OcrSuggestion>>> {
        let Some((_, db)) = self.open_knowledge_index(language)? else {
            return Ok(None);
        };
        let alphabet = alphabet(language);
        let mut known = db
            .prepare("SELECT EXISTS(SELECT 1 FROM forms WHERE term=?1)")
            .map_err(|error| error.to_string())?;
        let mut find = db
            .prepare(
                "SELECT f.term FROM json_each(?1) AS c
             JOIN forms f ON f.term=json_extract(c.value,'$[0]')
             GROUP BY f.term,json_extract(c.value,'$[1]')
             ORDER BY json_extract(c.value,'$[1]'),sum(f.observations) DESC,f.term LIMIT 3",
            )
            .map_err(|error| error.to_string())?;
        let mut results = Vec::new();
        let mut offset = 0;
        let mut unknown = 0;
        for part in text.split_word_bounds() {
            let start = offset;
            let len = part.encode_utf16().count();
            offset += len;
            if !(3..=32).contains(&len) || !part.chars().any(char::is_alphabetic) {
                continue;
            }
            let key = word_key(part);
            if known
                .query_row([&key], |row| row.get::<_, bool>(0))
                .map_err(|error| error.to_string())?
            {
                continue;
            }
            unknown += 1;
            let edits = candidate_edits(&key, &alphabet);
            let json = serde_json::to_string(&edits.into_iter().collect::<Vec<_>>())
                .map_err(|error| error.to_string())?;
            let candidates = find
                .query_map([&json], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            for candidate in candidates {
                let candidate = candidate.map_err(|error| error.to_string())?;
                results.push(OcrSuggestion {
                    start_utf16: start,
                    length_utf16: len,
                    source: part.to_owned(),
                    replacement: preserve_case(part, &candidate),
                });
            }
            if unknown >= 12 || results.len() >= 12 {
                break;
            }
        }
        results.truncate(12);
        Ok(Some(results))
    }
}

fn alphabet(language: &str) -> String {
    match language {
        "uk" => UKRAINIAN.to_owned(),
        "fr" => format!("{LATIN}{FRENCH}"),
        _ => LATIN.to_owned(),
    }
}

fn candidate_edits(key: &str, alphabet: &str) -> BTreeMap<String, u8> {
    let mut candidates = BTreeMap::new();
    let edges: Vec<usize> = key
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(key.len()))
        .collect();
    for (i, &start) in edges.iter().enumerate() {
        let prefix = &key[..start];
        let tail = &key[start..];
        for letter in alphabet.chars() {
            candidates.insert(format!("{prefix}{letter}{tail}"), 2);
        }
        if let Some(&end) = edges.get(i + 1) {
            let suffix = &key[end..];
            candidates.insert(format!("{prefix}{suffix}"), 2);
            for letter in alphabet.chars() {
                candidates.insert(format!("{prefix}{letter}{suffix}"), 2);
            }
            if let Some(&next) = edges.get(i + 2) {
                candidates.insert(
                    format!(
                        "{prefix}{}{}{}",
                        &key[end..next],
                        &key[start..end],
                        &key[next..]
                    ),
                    2,
                );
            }
        }
    }
    for &(from, to) in CONFUSIONS {
        for (wrong, right) in [(from, to), (to, from)] {
            for (index, _) in key.match_indices(wrong) {
                candidates.insert(
                    format!("{}{right}{}", &key[..index], &key[index + wrong.len()..]),
                    1,
                );
            }
        }
    }
    candidates.remove(key);
    candidates
}

fn preserve_case(source: &str, candidate: &str) -> String {
    let mut word = if source
        .chars()
        .filter(|letter| letter.is_alphabetic())
        .all(char::is_uppercase)
    {
        candidate.to_uppercase()
    } else if source.starts_with(char::is_uppercase) {
        let mut letters = candidate.chars();
        letters
            .next()
            .into_iter()
            .flat_map(char::to_uppercase)
            .chain(letters)
            .collect()
    } else {
        candidate.to_string()
    };
    if let Some(apostrophe) = source
        .chars()
        .find(|letter| matches!(letter, '\u{2019}' | '\u{02bc}'))
    {
        word = word.replace('\'', &apostrophe.to_string());
    }
    word
}
