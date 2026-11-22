use super::{word_key, WordContext};
use crate::Result;
use rusqlite::{params, Connection, Statement};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_PARTICLE_GAP: usize = 6;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionMatch {
    text: String,
    canonical: String,
    kind: String,
    observations: u32,
}

struct CorpusToken {
    id: u32,
    word: String,
    lemma: String,
    head: u32,
    relation: Option<&'static str>,
}

pub(super) struct Indexer<'a> {
    insert: Statement<'a>,
    tokens: Vec<CorpusToken>,
}

impl<'a> Indexer<'a> {
    pub(super) fn new(db: &'a Connection) -> Result<Self> {
        let insert = db
            .prepare(
                "INSERT INTO expressions VALUES (?1,?2,?3,1)
            ON CONFLICT DO UPDATE SET observations=observations+1",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            insert,
            tokens: Vec::new(),
        })
    }

    pub(super) fn token(&mut self, id: u32, form: &str, lemma: &str, head: u32, relation: &str) {
        self.tokens.push(CorpusToken {
            id,
            word: word_key(form),
            lemma: word_key(lemma),
            head,
            relation: match relation {
                "fixed" => Some("fixed"),
                "compound:prt" => Some("particle"),
                _ => None,
            },
        });
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        let tokens = std::mem::take(&mut self.tokens);
        if !tokens.iter().any(|token| token.relation.is_some()) {
            return Ok(());
        }
        let by_id: BTreeMap<_, _> = tokens.iter().map(|token| (token.id, token)).collect();
        let mut groups: BTreeMap<u32, Vec<&CorpusToken>> = BTreeMap::new();
        for token in &tokens {
            let Some(head) = by_id.get(&token.head) else {
                continue;
            };
            match token.relation {
                Some("fixed") => groups
                    .entry(head.id)
                    .or_insert_with(|| vec![head])
                    .push(token),
                Some("particle") if head.id < token.id && head.lemma != "_" => {
                    self.insert("particle", &[head.lemma.as_str(), token.word.as_str()])?;
                }
                _ => {}
            }
        }
        for mut group in groups.into_values() {
            group.sort_unstable_by_key(|token| token.id);
            if group.len() <= 8 && group.windows(2).all(|pair| pair[1].id == pair[0].id + 1) {
                let parts: Vec<_> = group.iter().map(|token| token.word.as_str()).collect();
                self.insert("fixed", &parts)?;
            }
        }
        Ok(())
    }

    fn insert(&mut self, kind: &str, parts: &[&str]) -> Result<()> {
        if parts
            .iter()
            .any(|part| !part.chars().any(char::is_alphabetic))
        {
            return Ok(());
        }
        let encoded = serde_json::to_string(parts).map_err(|error| error.to_string())?;
        self.insert
            .execute(params![parts[0], kind, encoded])
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

struct Token<'a> {
    text: &'a str,
    key: String,
    verb_lemmas: BTreeSet<String>,
}

pub(super) fn lookup(
    db: &Connection,
    context: &WordContext,
    selected_key: &str,
) -> Result<Vec<ExpressionMatch>> {
    let mut parts = context.parts();
    let mut before = VecDeque::with_capacity(8);
    let Some((start, clicked)) = parts.by_ref().find(|(start, part)| {
        if *start >= context.start_utf16 {
            return true;
        }
        if before.len() == 8 {
            before.pop_front();
        }
        before.push_back(*part);
        false
    }) else {
        return Ok(Vec::new());
    };
    if start != context.start_utf16 || word_key(clicked) != selected_key {
        return Ok(Vec::new());
    }
    let selected = before.len();
    let mut window: Vec<_> = before
        .into_iter()
        .chain(std::iter::once(clicked))
        .chain(parts.take(8).map(|(_, part)| part))
        .map(|part| Token {
            text: part,
            key: word_key(part),
            verb_lemmas: BTreeSet::new(),
        })
        .collect();
    let mut lemmas = db
        .prepare("SELECT DISTINCT lemma FROM forms WHERE term=?1 AND pos IN ('VERB','AUX')")
        .map_err(|error| error.to_string())?;
    for token in &mut window {
        if !token.key.chars().any(char::is_alphabetic) {
            continue;
        }
        let rows = lemmas
            .query_map([&token.key], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        for lemma in rows {
            token
                .verb_lemmas
                .insert(word_key(&lemma.map_err(|error| error.to_string())?));
        }
    }
    let anchors: BTreeSet<_> = window
        .iter()
        .flat_map(|token| {
            std::iter::once(token.key.as_str()).chain(token.verb_lemmas.iter().map(String::as_str))
        })
        .collect();
    let needles = serde_json::to_string(&anchors).map_err(|error| error.to_string())?;
    let mut query = db
        .prepare(
            "SELECT kind,parts,observations FROM json_each(?1) terms
        JOIN expressions e ON e.anchor=terms.value",
        )
        .map_err(|error| error.to_string())?;
    let rows = query
        .query_map([&needles], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u32>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut matches = BTreeMap::<(String, String, String), u32>::new();
    for row in rows {
        let (kind, parts, observations) = row.map_err(|error| error.to_string())?;
        let parts: Vec<String> = serde_json::from_str(&parts).map_err(|error| error.to_string())?;
        let canonical = parts.join(" ");
        for start in 0..=selected {
            if let Some(text) = match_expression(&window, &parts, &kind, start, selected) {
                let count = matches
                    .entry((text, kind.clone(), canonical.clone()))
                    .or_default();
                *count = (*count).max(observations);
            }
        }
    }
    let mut matches: Vec<_> = matches
        .into_iter()
        .map(|((text, kind, canonical), observations)| ExpressionMatch {
            text,
            canonical,
            kind,
            observations,
        })
        .collect();
    matches.sort_by(|left, right| {
        right
            .observations
            .cmp(&left.observations)
            .then_with(|| left.text.cmp(&right.text))
    });
    matches.truncate(4);
    Ok(matches)
}

fn match_expression(
    tokens: &[Token],
    parts: &[String],
    kind: &str,
    start: usize,
    selected: usize,
) -> Option<String> {
    if kind == "fixed" {
        let range = tokens.get(start..start + parts.len())?;
        return (selected < start + parts.len()
            && range
                .iter()
                .zip(parts)
                .all(|(token, part)| token.key == *part))
        .then(|| {
            range
                .iter()
                .map(|token| token.text)
                .collect::<Vec<_>>()
                .join(" ")
        });
    }
    let [verb, particle] = parts else {
        return None;
    };
    if !tokens[start].verb_lemmas.contains(verb) {
        return None;
    }
    let mut after = tokens
        .iter()
        .enumerate()
        .skip(start + 1)
        .take(MAX_PARTICLE_GAP + 1)
        .take_while(|(_, token)| token.key.chars().any(char::is_alphanumeric));
    let (end, matched) = after.find(|(index, token)| {
        token.key == *particle && (selected == start || *index == selected)
    })?;
    let separator = if end == start + 1 { " " } else { " … " };
    Some(format!("{}{separator}{}", tokens[start].text, matched.text))
}
