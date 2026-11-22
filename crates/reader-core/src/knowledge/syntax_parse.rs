use super::{syntax::WINDOW, word_key};
use crate::{document_tokens::word_parts, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::cmp::Reverse;

const MAX_WORDS: usize = 48;
const MAX_POS_TAGS: usize = 2;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SyntaxParse {
    status: ParseStatus,
    root_token_id: Option<String>,
    links: Vec<Dependency>,
    unassigned_token_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum ParseStatus {
    Complete,
    Partial,
    NoEvidence,
    TooLong,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Dependency {
    dependent_token_id: String,
    head_token_id: String,
    relation: String,
    basis: Evidence,
    observations: u32,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum Evidence {
    Lexical,
    PartOfSpeech,
}

struct Pattern {
    relation: String,
    observations: u32,
    basis: Evidence,
    rank: u32,
}

struct Edge {
    head: usize,
    dependent: usize,
    pattern: Pattern,
}

struct Token {
    offset: usize,
    term: String,
    tags: Vec<String>,
}

pub(super) fn analyze(
    db: &Connection,
    sentence: &str,
    id_at: impl Fn(usize) -> String,
) -> Result<SyntaxParse> {
    let words: Vec<_> = word_parts(sentence).take(MAX_WORDS + 1).collect();
    if words.len() > MAX_WORDS {
        return Ok(SyntaxParse {
            status: ParseStatus::TooLong,
            root_token_id: None,
            links: Vec::new(),
            unassigned_token_ids: Vec::new(),
        });
    }

    let mut tokens = Vec::with_capacity(words.len());
    let mut roots = Vec::new();
    let mut tags = db.prepare(
        "SELECT pos FROM forms WHERE term=?1 GROUP BY pos
         ORDER BY sum(observations) DESC, pos LIMIT ?2",
    ).map_err(database_error)?;
    let mut roots_by_pos = db.prepare(
        "SELECT coalesce(sum(CASE WHEN term=?2 THEN observations END),0),sum(observations)
         FROM syntax_roots WHERE pos=?1 HAVING count(*)>0",
    ).map_err(database_error)?;
    for (_, offset, text) in words {
        let term = word_key(text);
        let mut candidates = tags.query_map(params![term, MAX_POS_TAGS as u32], |row| row.get::<_, String>(0))
            .map_err(database_error)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(database_error)?;
        if candidates.is_empty() && text.chars().all(char::is_numeric) {
            candidates.push("NUM".to_owned());
        }
        for (index, candidate) in candidates.iter().enumerate() {
            let counts = roots_by_pos.query_row(
                params![candidate, term],
                |row| Ok((row.get::<_, u32>(0)?, row.get::<_, u32>(1)?)),
            ).optional().map_err(database_error)?;
            if let Some((exact, total)) = counts {
                let rank = if exact > 0 {
                    100 + (exact.ilog2() + 1) * 6
                } else {
                    total.checked_ilog2().unwrap_or(0) * 2
                };
                roots.push((rank.saturating_sub(index as u32 * 6), tokens.len()));
            }
        }
        tokens.push(Token { offset, term, tags: candidates });
    }
    let Some((_, root)) = roots.into_iter().max_by_key(|&(rank, index)| (rank, Reverse(index))) else {
        return Ok(SyntaxParse {
            status: ParseStatus::NoEvidence,
            root_token_id: None,
            links: Vec::new(),
            unassigned_token_ids: tokens.iter().map(|token| id_at(token.offset)).collect(),
        });
    };

    let mut exact = db.prepare(
        "SELECT relation,observations FROM syntax_links
         WHERE dependent=?1 AND head=?2 AND dependent_first=?3 AND distance=?4
         ORDER BY observations DESC,relation LIMIT 1",
    ).map_err(database_error)?;
    let mut general = db.prepare(
        "SELECT relation,observations FROM syntax_pos_links
         WHERE dependent=?1 AND head=?2 AND dependent_first=?3 AND distance=?4
         ORDER BY observations DESC,relation LIMIT 1",
    ).map_err(database_error)?;
    let mut edges = Vec::new();
    for (dependent, word) in tokens.iter().enumerate() {
        if dependent == root {
            continue;
        }
        for (head, governor) in tokens.iter().enumerate().filter(|(head, _)| *head != dependent) {
            let distance = dependent.abs_diff(head);
            let forward = dependent < head;
            let mut pattern = if distance <= WINDOW {
                observed_pattern(
                    &mut exact, &word.term, &governor.term, forward, distance, Evidence::Lexical,
                )?
            } else {
                None
            };
            if pattern.is_none() {
                for (left, left_tag) in word.tags.iter().enumerate() {
                    for (right, right_tag) in governor.tags.iter().enumerate() {
                        if let Some(mut found) = observed_pattern(
                            &mut general, left_tag, right_tag, forward, distance, Evidence::PartOfSpeech,
                        )? {
                            found.rank = found.rank.saturating_sub((left + right) as u32 * 4);
                            if pattern.as_ref().is_none_or(|current| found.rank > current.rank) {
                                pattern = Some(found);
                            }
                        }
                    }
                }
            }
            if let Some(pattern) = pattern {
                edges.push(Edge { head, dependent, pattern });
            }
        }
    }
    edges.sort_unstable_by_key(|edge| (
        Reverse(edge.pattern.rank), edge.dependent.abs_diff(edge.head), edge.dependent, edge.head,
    ));

    // Attach each word once to an already connected head. All retained arcs reach
    // the root, so cycles and a second root cannot enter the result.
    let mut connected = vec![false; tokens.len()];
    connected[root] = true;
    let mut links = Vec::new();
    while let Some(edge) = edges.iter().find(|edge| connected[edge.head] && !connected[edge.dependent]) {
        connected[edge.dependent] = true;
        links.push(Dependency {
            dependent_token_id: id_at(tokens[edge.dependent].offset),
            head_token_id: id_at(tokens[edge.head].offset),
            relation: edge.pattern.relation.clone(),
            basis: edge.pattern.basis,
            observations: edge.pattern.observations,
        });
    }
    let unassigned_token_ids: Vec<_> = tokens.iter().zip(connected)
        .filter_map(|(token, connected)| (!connected).then(|| id_at(token.offset)))
        .collect();
    Ok(SyntaxParse {
        status: if unassigned_token_ids.is_empty() { ParseStatus::Complete } else { ParseStatus::Partial },
        root_token_id: Some(id_at(tokens[root].offset)),
        links,
        unassigned_token_ids,
    })
}

fn observed_pattern(
    query: &mut rusqlite::Statement<'_>,
    dependent: &str,
    head: &str,
    forward: bool,
    distance: usize,
    basis: Evidence,
) -> Result<Option<Pattern>> {
    query.query_row(params![dependent, head, forward, distance.min(WINDOW + 1) as u32], |row| {
        let observations = row.get::<_, u32>(1)?;
        let base = match basis { Evidence::Lexical => 100, Evidence::PartOfSpeech => 10 };
        Ok(Pattern {
            relation: row.get(0)?,
            observations,
            basis,
            rank: base + observations.checked_ilog2().unwrap_or(0) * 4,
        })
    }).optional().map_err(database_error)
}

fn database_error(error: impl std::fmt::Display) -> String {
    format!("Помилка синтаксичного аналізу. {error}")
}
