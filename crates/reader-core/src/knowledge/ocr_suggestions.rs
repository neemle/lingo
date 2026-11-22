use super::{neighbor_hash, word_key};
use crate::{document_tokens::word_parts, ocr::OcrSpan, Result, Sentence, Store};
use rusqlite::{params, Connection, Statement, ToSql};
use serde::Serialize;
use std::collections::BTreeMap;
use unicode_normalization::char::is_combining_mark;
use unicode_segmentation::UnicodeSegmentation;

const LATIN: &str = "abcdefghijklmnopqrstuvwxyz'";
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SpacingSuggestion {
    span: OcrSpan,
    source: String,
    replacement: String,
}

pub(super) fn spacing_suggestions(
    db: &Connection,
    sentence: &Sentence,
    selected: &[usize],
) -> Result<Vec<SpacingSuggestion>> {
    let Some(&last_selected) = selected.last() else {
        return Ok(Vec::new());
    };
    let mut attested = db
        .prepare("SELECT EXISTS(SELECT 1 FROM forms WHERE term=?1)")
        .map_err(|error| error.to_string())?;
    let mut neighbors = db
        .prepare("SELECT EXISTS(SELECT 1 FROM neighbors WHERE term=?1 AND side=1 AND neighbor=?2)")
        .map_err(|error| error.to_string())?;
    let mut previous: Option<(usize, usize, &str)> = None;
    let mut suggestions = Vec::new();
    let is_selected = |offset| selected.binary_search(&offset).is_ok();
    for (byte, offset, word) in word_parts(&sentence.text) {
        if let Some((left_byte, left_offset, left)) = previous {
            if (is_selected(left_offset) || is_selected(offset))
                && word_letters(left)
                && word_letters(word)
                && left_byte + left.len() < byte
                && sentence.text[left_byte + left.len()..byte]
                    .chars()
                    .all(char::is_whitespace)
                && left.len() + word.len() <= 96
            {
                let replacement = format!("{left}{word}");
                if replacement.graphemes(true).nth(3).is_some()
                    && attested_form(&mut attested, &replacement)?
                    && (!attested_form(&mut attested, left)?
                        || !attested_form(&mut attested, word)?)
                {
                    suggestions.push(spacing_hint(
                        sentence,
                        left_byte,
                        left_offset,
                        byte + word.len(),
                        replacement,
                    ));
                }
            }
        }
        if offset > last_selected {
            break;
        }
        if is_selected(offset)
            && word_letters(word)
            && word.len() <= 96
            && !attested_form(&mut attested, word)?
        {
            if let Some(replacement) = unique_split(&mut neighbors, word)? {
                suggestions.push(spacing_hint(
                    sentence,
                    byte,
                    offset,
                    byte + word.len(),
                    replacement,
                ));
            }
        }
        previous = Some((byte, offset, word));
    }
    suggestions.sort_unstable_by_key(|suggestion| suggestion.span.start_utf8);
    Ok(suggestions)
}

fn attested_form(query: &mut Statement<'_>, form: &str) -> Result<bool> {
    query
        .query_row([word_key(form)], |row| row.get(0))
        .map_err(|error| error.to_string())
}

fn unique_split(query: &mut Statement<'_>, word: &str) -> Result<Option<String>> {
    let mut result = None;
    for (byte, _) in word.grapheme_indices(true).skip(2) {
        let (left, right) = word.split_at(byte);
        if right.graphemes(true).nth(1).is_none() {
            break;
        }
        let observed = query
            .query_row(
                params![word_key(left), neighbor_hash(&word_key(right))],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| error.to_string())?;
        if observed {
            if result.is_some() {
                return Ok(None);
            }
            result = Some(format!("{left} {right}"));
        }
    }
    Ok(result)
}

fn word_letters(word: &str) -> bool {
    word.chars()
        .all(|letter| letter.is_alphabetic() || is_combining_mark(letter))
}

fn spacing_hint(
    sentence: &Sentence,
    start_byte: usize,
    start_utf16: usize,
    end_byte: usize,
    replacement: String,
) -> SpacingSuggestion {
    let source = &sentence.text[start_byte..end_byte];
    SpacingSuggestion {
        span: OcrSpan {
            start_utf8: sentence.start_utf8 + start_byte,
            end_utf8: sentence.start_utf8 + end_byte,
            start_utf16: sentence.start_utf16 + start_utf16,
            end_utf16: sentence.start_utf16 + start_utf16 + source.encode_utf16().count(),
        },
        source: source.to_owned(),
        replacement,
    }
}

/// Only change a missing form if exactly one candidate is observed beside both source neighbors.
pub(super) fn revise_line(db: &Connection, language: &str, text: &str) -> Result<Option<String>> {
    let words: Vec<_> = word_parts(text).take(120).collect();
    let mut attested = db.prepare_cached("SELECT EXISTS(SELECT 1 FROM forms WHERE term=?1)")
        .map_err(|error| error.to_string())?;
    let mut edited = None;
    for neighbors in words.windows(3).rev() {
        let [(_, _, left), (offset, _, word), (_, _, right)] = neighbors else { continue; };
        if !word.starts_with(char::is_lowercase) || word.chars().take(4).count() < 4 ||
            attested_form(&mut attested, word)? {
            continue;
        }

        let key = word_key(word);
        let previous = neighbor_hash(&word_key(left));
        let next = neighbor_hash(&word_key(right));
        let suggestions = suggest_spelling(db, language, word, &key, Some(previous), Some(next))?;
        if let Some(best) = suggestions.first().filter(|candidate| candidate.high_confidence) {
            edited.get_or_insert_with(|| text.to_owned())
                .replace_range(*offset..*offset + word.len(), &best.replacement);
        }
    }
    Ok(edited)
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
    let variant = language.split('-').next().unwrap_or(language);
    let diacritics = match variant {
        "uk" => return UKRAINIAN.to_owned(),
        "fr" => "àâäæçéèêëîïôöœùûüÿ",
        "de" => "äöüß",
        "es" => "áéíñóúü",
        "it" => "àèéìíîòóùú",
        "pt" => "àáâãçéêíóôõúü",
        "pl" => "ąćęłńóśźż",
        "nl" => "áàäéèëíïóöúü",
        "cs" => "áčďéěíňóřšťúůýž",
        "ro" => "ăâîșț",
        "sv" => "åäöé",
        _ => "",
    };
    format!("{LATIN}{diacritics}")
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
