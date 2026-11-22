use super::{
    expressions::ExpressionMatch, ocr_suggestions::SpellingSuggestion, versioned_index,
    KnowledgePackage, WordAnalysis, WordContext, PACKS,
};
use crate::{
    document_tokens::{find_sentence, revision_id, token_id, word_parts},
    ocr::OcrSpan,
    Result, Store,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const SENTENCE_PAGE_SIZE: usize = 24;
const MAX_READINGS: usize = 8;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum AnalysisScope {
    Word {
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
    },
    Expression {
        #[serde(rename = "memberStartsUtf16")]
        member_starts_utf16: Vec<usize>,
    },
    Sentence {
        #[serde(default)]
        start: usize,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinguisticAnalysis {
    revision_id: String,
    sentence_id: String,
    language: String,
    package: Option<KnowledgePackage>,
    words: Vec<AnalyzedWord>,
    expressions: Vec<ExpressionMatch>,
    syntax_candidates: Vec<SyntaxCandidate>,
    next_start: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalyzedWord {
    token_id: String,
    text: String,
    /// Global canonical-document offsets.
    span: OcrSpan,
    sentence_start_utf16: usize,
    morphology: Vec<WordAnalysis>,
    spelling_status: SpellingStatus,
    spelling_suggestions: Vec<SpellingSuggestion>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
enum SpellingStatus {
    Attested,
    Suggested,
    Unattested,
    Unavailable,
    NotApplicable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SyntaxCandidate {
    dependent_token_id: String,
    head_token_id: String,
    relation: String,
    observations: u32,
}

impl Store {
    pub fn analyze_linguistics(
        &self,
        document_id: &str,
        sentence_id: &str,
        expected_revision: &str,
        scope: AnalysisScope,
    ) -> Result<LinguisticAnalysis> {
        let document = self.token_document(document_id)?;
        let revision = revision_id(&document)?;
        if revision != expected_revision {
            return Err("Текст документа змінився. Оновіть запит аналізу.".into());
        }
        let sentence = find_sentence(&document, sentence_id)?;
        let (selected, next_start) = select_words(&sentence.text, &scope)?;
        let language = document.language.as_str();
        let index = self.open_knowledge_index(language)?;
        let lookup_index = versioned_index(&index)?;
        let package = PACKS.iter().find(|pack| pack.language == language).map(|pack| {
            pack.describe(lookup_index.map_or(0, |(_, _, version)| version))
        });
        let id_at = |offset| token_id(revision, sentence_id, offset);
        let members = match &scope {
            AnalysisScope::Expression { member_starts_utf16 } => Some(member_starts_utf16),
            _ => None,
        };
        let mut words = Vec::with_capacity(selected.len());
        let mut expressions = BTreeMap::new();
        let mut dependencies = BTreeMap::new();
        let mut context = WordContext::new(sentence.text.clone(), 0);
        for (byte, offset, text) in selected {
            context.start_utf16 = offset;
            let alphabetic = text.chars().any(char::is_alphabetic);
            let mut spelling_status = if alphabetic {
                SpellingStatus::Unavailable
            } else {
                SpellingStatus::NotApplicable
            };
            let mut morphology = Vec::new();
            let mut spelling_suggestions = Vec::new();
            if alphabetic {
                if let Some(mut lookup) =
                    self.lookup_word_with_index(language, text, Some(&context), lookup_index)?
                {
                    lookup.bind_expression_tokens(id_at);
                    spelling_status = if !lookup.analyses.is_empty() || !lookup.meanings.is_empty() {
                        SpellingStatus::Attested
                    } else if !lookup.spelling_suggestions.is_empty() {
                        SpellingStatus::Suggested
                    } else {
                        SpellingStatus::Unattested
                    };
                    morphology = lookup.analyses;
                    morphology.truncate(MAX_READINGS);
                    spelling_suggestions = lookup.spelling_suggestions;
                    for expression in lookup.expressions {
                        if members.is_none_or(|members| {
                            members.as_slice() == expression.part_starts_utf16.as_slice()
                        }) {
                            let key = (expression.part_starts_utf16.clone(), expression.canonical.clone(), expression.kind.clone());
                            expressions.entry(key).or_insert(expression);
                        }
                    }
                    for edge in lookup.syntax_evidence {
                        if members.is_some_and(|members| members.binary_search(&edge.other_start_utf16).is_err()) {
                            continue;
                        }
                        let (dependent, head) = if edge.selected_is_head {
                            (edge.other_start_utf16, offset)
                        } else {
                            (offset, edge.other_start_utf16)
                        };
                        dependencies.entry((dependent, head, edge.relation))
                            .and_modify(|count: &mut u32| *count = (*count).max(edge.observations))
                            .or_insert(edge.observations);
                    }
                }
            }
            words.push(AnalyzedWord {
                token_id: id_at(offset),
                text: text.to_owned(),
                span: OcrSpan {
                    start_utf8: sentence.start_utf8 + byte,
                    end_utf8: sentence.start_utf8 + byte + text.len(),
                    start_utf16: sentence.start_utf16 + offset,
                    end_utf16: sentence.start_utf16 + offset + text.encode_utf16().count(),
                },
                sentence_start_utf16: offset,
                morphology,
                spelling_status,
                spelling_suggestions,
            });
        }
        Ok(LinguisticAnalysis {
            revision_id: revision.to_owned(),
            sentence_id: sentence_id.to_owned(),
            language: language.to_owned(),
            package,
            words,
            expressions: expressions.into_values().collect(),
            syntax_candidates: dependencies.into_iter()
                .map(|((dependent, head, relation), observations)| SyntaxCandidate {
                    dependent_token_id: id_at(dependent),
                    head_token_id: id_at(head),
                    relation,
                    observations,
                })
                .collect(),
            next_start,
        })
    }
}

type WordPart<'a> = (usize, usize, &'a str);

fn select_words<'a>(
    sentence: &'a str, scope: &AnalysisScope,
) -> Result<(Vec<WordPart<'a>>, Option<usize>)> {
    if let AnalysisScope::Sentence { start } = scope {
        let mut selected: Vec<_> = word_parts(sentence).skip(*start).take(SENTENCE_PAGE_SIZE + 1).collect();
        let next_start = if selected.len() > SENTENCE_PAGE_SIZE {
            selected.pop();
            Some(*start + SENTENCE_PAGE_SIZE)
        } else {
            None
        };
        return Ok((selected, next_start));
    }
    let offsets = match scope {
        AnalysisScope::Word { start_utf16 } => std::slice::from_ref(start_utf16),
        AnalysisScope::Expression { member_starts_utf16 } => {
            if !(2..=8).contains(&member_starts_utf16.len())
                || !member_starts_utf16.windows(2).all(|pair| pair[0] < pair[1])
            {
                return Err("Вираз має містити від двох до восьми слів у порядку речення.".into());
            }
            member_starts_utf16
        }
        AnalysisScope::Sentence { .. } => unreachable!(),
    };
    let last = offsets.last().copied().unwrap_or(0);
    let selected: Vec<_> = word_parts(sentence)
        .take_while(|(_, offset, _)| *offset <= last)
        .filter(|(_, offset, _)| offsets.binary_search(offset).is_ok())
        .collect();
    if selected.len() != offsets.len() {
        return Err("Вибране слово або частину виразу не знайдено в реченні.".into());
    }
    Ok((selected, None))
}
