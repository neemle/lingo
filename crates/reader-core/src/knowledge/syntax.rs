use super::{word_key, WordContext};
use crate::Result;
use rusqlite::{params, Connection, Statement};
use serde::Serialize;
use std::{cmp::Reverse, collections::VecDeque};

const WINDOW: usize = 6;
const MAX_HINTS: usize = 5;

pub(super) const SCHEMA: &str = "CREATE TABLE syntax_links (
    dependent TEXT NOT NULL, head TEXT NOT NULL, dependent_first INTEGER NOT NULL,
    distance INTEGER NOT NULL, relation TEXT NOT NULL, observations INTEGER NOT NULL,
    PRIMARY KEY(dependent,head,dependent_first,distance,relation)
) WITHOUT ROWID;";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntaxEvidence {
    relation: String,
    selected_is_head: bool,
    other_word: String,
    other_start_utf16: usize,
    observations: u32,
}

struct Token {
    id: u32,
    head: u32,
    key: String,
    relation: Option<String>,
}

pub(super) struct Indexer<'a> {
    upsert: Statement<'a>,
    tokens: Vec<Token>,
}

impl<'a> Indexer<'a> {
    pub(super) fn new(db: &'a Connection) -> Result<Self> {
        let upsert = db
            .prepare(
                "INSERT INTO syntax_links VALUES (?1,?2,?3,?4,?5,1)
                 ON CONFLICT DO UPDATE SET observations=observations+1",
            )
            .map_err(|error| error.to_string())?;
        Ok(Self {
            upsert,
            tokens: Vec::new(),
        })
    }

    // CoNLL-U columns HEAD and DEPREL contain the basic dependency edge.
    pub(super) fn token(&mut self, id: u32, form: &str, pos: &str, head: u32, relation: &str) {
        if !eligible(form) || matches!(pos, "_" | "X" | "PUNCT" | "SYM") {
            return;
        }
        let base = relation.split(':').next().unwrap_or_default();
        let relation = matches!(
            base,
            "acl"
                | "advcl"
                | "advmod"
                | "amod"
                | "appos"
                | "aux"
                | "case"
                | "cc"
                | "ccomp"
                | "compound"
                | "conj"
                | "cop"
                | "csubj"
                | "det"
                | "expl"
                | "fixed"
                | "flat"
                | "iobj"
                | "mark"
                | "nmod"
                | "nsubj"
                | "nummod"
                | "obj"
                | "obl"
                | "xcomp"
        )
        .then(|| relation.to_owned());
        self.tokens.push(Token {
            id,
            head,
            key: word_key(form),
            relation,
        });
    }

    pub(super) fn flush(&mut self) -> Result<()> {
        let mut tokens = std::mem::take(&mut self.tokens);
        tokens.sort_unstable_by_key(|token| token.id);
        for (dependent_index, dependent) in tokens.iter().enumerate() {
            let Some(relation) = &dependent.relation else {
                continue;
            };
            if dependent.head == 0 || dependent.id == dependent.head {
                continue;
            }
            let Ok(index) = tokens.binary_search_by_key(&dependent.head, |token| token.id) else {
                continue;
            };
            let distance = dependent_index.abs_diff(index);
            if distance > WINDOW {
                continue;
            }
            let head = &tokens[index];
            self.upsert
                .execute(params![
                    dependent.key,
                    head.key,
                    dependent.id < head.id,
                    distance as u32,
                    relation
                ])
                .map_err(|error| error.to_string())?;
        }
        tokens.clear();
        self.tokens = tokens;
        Ok(())
    }
}

pub(super) fn lookup(
    db: &Connection,
    context: &WordContext,
    selected: &str,
) -> Result<Vec<SyntaxEvidence>> {
    let window = nearby(context, selected);
    if window.is_empty() {
        return Ok(Vec::new());
    }
    let mut query = db
        .prepare(
            "SELECT relation, observations, 0 FROM syntax_links
         WHERE dependent=?1 AND head=?2 AND dependent_first=?3 AND distance=?4
         UNION ALL
         SELECT relation, observations, 1 FROM syntax_links
         WHERE dependent=?2 AND head=?1 AND dependent_first!=?3 AND distance=?4",
        )
        .map_err(|error| error.to_string())?;
    let mut candidates = Vec::new();
    for (offset, word, distance) in window {
        let rows = query
            .query_map(
                params![
                    selected,
                    word_key(word),
                    context.start_utf16 < offset,
                    distance as u32
                ],
                |row| {
                    Ok(SyntaxEvidence {
                        relation: row.get(0)?,
                        observations: row.get(1)?,
                        selected_is_head: row.get(2)?,
                        other_word: word.to_owned(),
                        other_start_utf16: offset,
                    })
                },
            )
            .map_err(|error| error.to_string())?;
        for row in rows {
            candidates.push((distance, row.map_err(|error| error.to_string())?));
        }
    }
    candidates.sort_by(|(left_distance, left), (right_distance, right)| {
        (
            *left_distance,
            Reverse(left.observations),
            left.other_start_utf16,
            &left.relation,
            left.selected_is_head,
        )
            .cmp(&(
                *right_distance,
                Reverse(right.observations),
                right.other_start_utf16,
                &right.relation,
                right.selected_is_head,
            ))
    });
    Ok(candidates
        .into_iter()
        .take(MAX_HINTS)
        .map(|(_, hint)| hint)
        .collect())
}

fn nearby<'a>(context: &'a WordContext, selected: &str) -> Vec<(usize, &'a str, usize)> {
    let mut before = VecDeque::with_capacity(WINDOW);
    let mut after = Vec::with_capacity(WINDOW);
    let mut found = false;
    for (offset, part) in context.parts() {
        if offset == context.start_utf16 {
            if word_key(part) != selected {
                return Vec::new();
            }
            found = true;
            continue;
        }
        if !found && offset > context.start_utf16 {
            return Vec::new();
        }
        if part.contains([',', '.', ';', ':', '!', '?', '…']) {
            if found {
                break;
            }
            before.clear();
        }
        if !eligible(part) {
            continue;
        }
        if found {
            after.push((offset, part, after.len() + 1));
            if after.len() == WINDOW {
                break;
            }
        } else {
            if before.len() == WINDOW {
                before.pop_front();
            }
            before.push_back((offset, part));
        }
    }
    if !found {
        return Vec::new();
    }
    before
        .into_iter()
        .rev()
        .enumerate()
        .map(|(distance, (offset, word))| (offset, word, distance + 1))
        .chain(after)
        .collect()
}

fn eligible(form: &str) -> bool {
    form.len() <= 240 && form.chars().any(char::is_alphanumeric)
}
