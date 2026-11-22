use super::{content_words, lexical_error, Sense, MAX_SENSES_PER_TERM};
use crate::Result;
use rusqlite::{params, Connection, Statement};
use std::{collections::BTreeSet, fmt::Write};

pub(super) const SCHEMA: &str = "CREATE VIRTUAL TABLE meaning_search USING fts5(
    term_token, example_words, gloss_words, content='', detail=column, columnsize=0
);";
pub(super) const INSERT: &str =
    "INSERT INTO meaning_search(rowid,term_token,example_words,gloss_words)
    VALUES (?1,?2,?3,?4)";
const MAX_PATTERN_BYTES: usize = 8 * 1024;

pub(super) type Match = (i64, Option<String>);

pub(super) fn index(
    query: &mut Statement<'_>,
    language: &str,
    term: &str,
    ordinal: i64,
    sense: &Sense,
) -> Result<()> {
    let examples = tokens(
        sense
            .examples
            .iter()
            .flat_map(|example| content_words(language, &example.text)),
    );
    let gloss = tokens(content_words(language, &sense.gloss));
    query
        .execute(params![ordinal, token(term), examples, gloss])
        .map_err(lexical_error)?;
    Ok(())
}

pub(super) fn matching(
    db: &Connection,
    lemma: &str,
    after: i64,
    selected: &str,
    context: &BTreeSet<String>,
    gloss_anchor: Option<&str>,
) -> Result<Option<Vec<Match>>> {
    let mut matches = Vec::new();
    let examples = context
        .iter()
        .filter(|word| word.len() <= 800 && word.as_str() != lemma && word.as_str() != selected)
        .map(|word| token(word))
        .collect::<Vec<_>>();
    if !examples.is_empty() {
        matches.push(format!("example_words : ({})", examples.join(" OR ")));
    }
    if let Some(anchor) = gloss_anchor
        .filter(|anchor| anchor.len() <= 2_000 && *anchor != lemma && *anchor != selected)
    {
        matches.push(format!("gloss_words : {}", token(anchor)));
    }
    if matches.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let pattern = format!(
        "term_token : {} AND ({})",
        token(lemma),
        matches.join(" OR ")
    );
    if pattern.len() > MAX_PATTERN_BYTES {
        return Ok(None);
    }
    let mut query = db
        .prepare_cached(
            "SELECT m.ordinal, CASE WHEN m.ordinal>?2 THEN m.body END
             FROM meaning_search CROSS JOIN meanings m
             WHERE meaning_search MATCH ?1 AND m.term=?3 AND m.ordinal=meaning_search.rowid
             ORDER BY meaning_search.rowid LIMIT ?4",
        )
        .map_err(lexical_error)?;
    let matches = query
        .query_map(
            params![pattern, after, lemma, 2 * MAX_SENSES_PER_TERM],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(lexical_error)?;
    matches
        .collect::<rusqlite::Result<_>>()
        .map(Some)
        .map_err(lexical_error)
}

fn tokens(words: impl IntoIterator<Item = String>) -> String {
    words
        .into_iter()
        .map(|word| token(&word))
        .collect::<Vec<_>>()
        .join(" ")
}

// Feed the tokenizer reversible ASCII tokens to preserve the application's accent,
// apostrophe and Unicode word boundaries. The prefixes keep their encodings disjoint.
fn token(word: &str) -> String {
    if word
        .bytes()
        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return format!("a{word}");
    }
    let mut encoded = String::with_capacity(1 + 2 * word.len());
    encoded.push('u');
    for byte in word.bytes() {
        write!(encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}
