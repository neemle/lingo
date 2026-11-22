use super::{
    annotation_cache, expressions::ExpressionMatch, lexicon, ocr_suggestions::SpellingSuggestion,
    syntax_parse, versioned_index, KnowledgePackage, Pack, WordAnalysis, WordContext, PACKS,
};
use crate::{
    document_tokens::{find_sentence, revision_id, token_id, word_parts},
    ocr::OcrSpan,
    Document, Result, Sentence, Store,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const SENTENCE_PAGE_SIZE: usize = 24;
const MAX_READINGS: usize = 8;
const MAX_PREFETCH_SENTENCE_BYTES: usize = 4096;
// Bump when generated annotations or their serialized representation changes.
const ANNOTATION_VERSION: u32 = 1;

#[derive(Debug, Deserialize, Serialize)]
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

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum DetachedPageAnalysis {
    Sentence {
        sentence_id: String,
        #[serde(default)]
        start: usize,
    },
    Selection { start_utf16: usize, text: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetachedVocabularyInput {
    start_utf16: usize,
    selected_text: String,
    translation: String,
    target: String,
    analysis: Option<Box<crate::LemmaSnapshot>>,
    expression: Option<Box<crate::ExpressionSnapshot>>,
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
    syntax_parse: Option<syntax_parse::SyntaxParse>,
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
    dependent_text: String,
    head_text: String,
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
        let sentence = analysis_sentence(&document, sentence_id, expected_revision)?;
        let index = self.open_knowledge_index(&document.language)?;
        self.analyze_scope(
            &document.language, sentence, expected_revision, &scope, versioned_index(&index)?,
        )
    }

    pub fn cached_linguistic_analysis(
        &self,
        document_id: &str,
        sentence_id: &str,
        expected_revision: &str,
        scope: AnalysisScope,
    ) -> Result<Value> {
        let document = self.token_document(document_id)?;
        let sentence = analysis_sentence(&document, sentence_id, expected_revision)?;
        let language = &document.language;
        let index = self.open_knowledge_index(language)?;
        let lexical = lexicon::index_identity(&self.knowledge_directory, language);
        self.cached_scope(
            (&document.id, language), sentence, expected_revision, &scope, versioned_index(&index)?, lexical,
        )
    }

    pub fn cached_detached_pdf_analysis(
        &self, document_id: &str, page: u16, request: DetachedPageAnalysis,
    ) -> Result<Value> {
        let cached = self.detached_pdf_sentences(document_id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let (sentence, scope) = match request {
            DetachedPageAnalysis::Sentence { sentence_id, start } => {
                let sentence = cached.sentences.iter().find(|sentence| sentence.id == sentence_id)
                    .ok_or("Речення сторінки PDF змінилося.")?;
                (sentence, AnalysisScope::Sentence { start })
            }
            DetachedPageAnalysis::Selection { start_utf16, text } =>
                detached_selection(&cached.sentences, start_utf16, &text)?,
        };
        let index = self.open_knowledge_index(&cached.language)?;
        let lexical = lexicon::index_identity(&self.knowledge_directory, &cached.language);
        self.cached_scope(
            (document_id, &cached.language), sentence, &cached.revision, &scope,
            versioned_index(&index)?, lexical,
        )
    }

    pub fn save_detached_pdf_word(
        &mut self, document_id: &str, page: u16, input: DetachedVocabularyInput,
    ) -> Result<crate::Vocabulary> {
        let cached = self.detached_pdf_sentences(document_id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let (sentence, _) = detached_selection(&cached.sentences, input.start_utf16, &input.selected_text)?;
        self.save_word_for_document(crate::Vocabulary {
            id: String::new(),
            word: input.selected_text,
            translation: input.translation,
            language: cached.language,
            target_language: input.target,
            context: sentence.text.clone(),
            document_id: document_id.to_owned(),
            forward: Default::default(),
            reverse: Default::default(),
            analysis: input.analysis,
            expression: input.expression,
            sense: None,
            accepted_translations: Vec::new(),
        }, true)
    }

    /// Warm one sentence in the current reading window. A changed cursor or
    /// revision makes queued work obsolete.
    pub fn prefetch_linguistics(
        &self, document_id: &str, anchor_id: &str, expected_revision: &str, step: i8,
    ) -> Result<()> {
        if !(-1..=1).contains(&step) {
            return Err("Фоновий аналіз доступний лише для сусідніх речень.".into());
        }
        let document = self.token_document(document_id)?;
        if document.current_revision_id.as_deref() != Some(expected_revision)
            || document.sentences.get(document.position).map(|item| item.id.as_str()) != Some(anchor_id)
        {
            return Ok(());
        }
        let Some(position) = document.position.checked_add_signed(isize::from(step)) else {
            return Ok(());
        };
        let Some(sentence) = document.sentences.get(position)
            .filter(|sentence| sentence.text.len() <= MAX_PREFETCH_SENTENCE_BYTES)
        else {
            return Ok(());
        };
        let language = &document.language;
        let index = self.open_knowledge_index(language)?;
        let lookup_index = versioned_index(&index)?;
        let lexical = lexicon::index_identity(&self.knowledge_directory, language);
        if lookup_index.is_some() || lexical.is_some() {
            self.cached_scope(
                (&document.id, language), sentence, expected_revision, &AnalysisScope::Sentence { start: 0 },
                lookup_index, lexical,
            )?;
        }
        Ok(())
    }

    fn cached_scope(
        &self,
        source: (&str, &str),
        sentence: &Sentence,
        revision: &str,
        scope: &AnalysisScope,
        lookup_index: Option<(&Pack, &Connection, u32)>,
        lexical: Option<(&str, u32)>,
    ) -> Result<Value> {
        let (document_id, language) = source;
        let grammar = lookup_index.map(|(pack, _, version)| (pack.sha256, version));
        let address = serde_json::to_vec(&(
            ANNOTATION_VERSION, revision, &sentence.id, language, grammar, lexical, scope,
        ))
        .map_err(|error| error.to_string())?;
        let key = Sha256::digest(address);
        if let Some(annotation) = annotation_cache::read(&self.connection, document_id, key.as_ref()) {
            return Ok(annotation);
        }
        let analysis =
            self.analyze_scope(language, sentence, revision, scope, lookup_index)?;
        let annotation = serde_json::to_value(analysis).map_err(|error| error.to_string())?;
        if lexicon::index_identity(&self.knowledge_directory, language) == lexical {
            annotation_cache::write(&self.connection, document_id, key.as_ref(), &annotation);
        }
        Ok(annotation)
    }

    fn analyze_scope(
        &self,
        language: &str,
        sentence: &Sentence,
        revision: &str,
        scope: &AnalysisScope,
        lookup_index: Option<(&Pack, &Connection, u32)>,
    ) -> Result<LinguisticAnalysis> {
        let (selected, next_start) = select_words(&sentence.text, scope)?;
        let sentence_id = sentence.id.as_str();
        let package = PACKS.iter().find(|pack| pack.language == language).map(|pack| {
            pack.describe(lookup_index.map_or(0, |(_, _, version)| version))
        });
        let id_at = |offset| token_id(revision, sentence_id, offset);
        let syntax_parse = match (scope, lookup_index) {
            (AnalysisScope::Sentence { start: 0 }, Some((_, db, version))) if version >= 5 => {
                Some(syntax_parse::analyze(db, &sentence.text, id_at)?)
            }
            _ => None,
        };
        let members = match scope {
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
                        let (dependent, head, dependent_text, head_text) = if edge.selected_is_head {
                            (edge.other_start_utf16, offset, edge.other_word.as_str(), text)
                        } else {
                            (offset, edge.other_start_utf16, text, edge.other_word.as_str())
                        };
                        dependencies.entry((dependent, head, edge.relation.clone()))
                            .and_modify(|candidate: &mut SyntaxCandidate| {
                                candidate.observations = candidate.observations.max(edge.observations);
                            })
                            .or_insert_with(|| SyntaxCandidate {
                                dependent_token_id: id_at(dependent),
                                head_token_id: id_at(head),
                                dependent_text: dependent_text.to_owned(),
                                head_text: head_text.to_owned(),
                                relation: edge.relation,
                                observations: edge.observations,
                            });
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
            syntax_candidates: dependencies.into_values().collect(),
            next_start,
            syntax_parse,
        })
    }
}

fn detached_selection<'a>(
    sentences: &'a [Sentence], start_utf16: usize, selected: &str,
) -> Result<(&'a Sentence, AnalysisScope)> {
    const MAX_SELECTION_BYTES: usize = 4096;
    if selected.is_empty() || selected.len() > MAX_SELECTION_BYTES {
        return Err("Виділіть слово або вираз до восьми слів.".into());
    }
    let left_trimmed = selected.trim_start();
    let prefix = selected.len() - left_trimmed.len();
    let start_utf16 = start_utf16.checked_add(selected[..prefix].encode_utf16().count())
        .ok_or("Виділення сторінки PDF завелике.")?;
    let selected = left_trimmed.trim_end();
    let end_utf16 = start_utf16.checked_add(selected.encode_utf16().count())
        .ok_or("Виділення сторінки PDF завелике.")?;
    let sentence = sentences.iter().find(|sentence| {
        sentence.start_utf16 <= start_utf16 && sentence.end_utf16 >= end_utf16
    }).ok_or("Виділення має бути в межах одного речення.")?;
    let start = start_utf16 - sentence.start_utf16;
    let end = end_utf16 - sentence.start_utf16;
    let (Some(first), Some(last)) = (
        crate::ocr::byte_at_utf16(&sentence.text, start),
        crate::ocr::byte_at_utf16(&sentence.text, end),
    ) else {
        return Err("Некоректна позиція виділення PDF.".into());
    };
    if sentence.text.get(first..last) != Some(selected) {
        return Err("Текст виділення на сторінці PDF змінився.".into());
    }
    let members: Vec<_> = word_parts(&sentence.text)
        .take_while(|(_, offset, _)| *offset < end)
        .filter(|(_, offset, text)| *offset + text.encode_utf16().count() > start)
        .map(|(_, offset, _)| offset)
        .take(9)
        .collect();
    let scope = match members.as_slice() {
        [start_utf16] => AnalysisScope::Word { start_utf16: *start_utf16 },
        members if (2..=8).contains(&members.len()) => AnalysisScope::Expression {
            member_starts_utf16: members.to_vec(),
        },
        _ => return Err("Виділіть слово або вираз до восьми слів.".into()),
    };
    Ok((sentence, scope))
}

fn analysis_sentence<'a>(
    document: &'a Document, sentence_id: &str, expected_revision: &str,
) -> Result<&'a Sentence> {
    if revision_id(document)? != expected_revision {
        return Err("Текст документа змінився. Оновіть запит аналізу.".into());
    }
    find_sentence(document, sentence_id)
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
