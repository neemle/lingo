use super::{
    annotation_cache, expressions::ExpressionMatch, lexicon, ocr_suggestions::{spacing_suggestions, SpacingSuggestion, SpellingSuggestion},
    syntax_parse, versioned_index, KnowledgePackage, Pack, WordAnalysis, WordContext, WordLookup, PACKS,
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
const SENTENCE_ANNOTATION_VERSION: u32 = 5;
const SELECTION_ANNOTATION_VERSION: u32 = 6;

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
    Expression {
        sentence_id: String,
        revision_id: String,
        member_starts_utf16: Vec<usize>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DetachedVocabularyInput {
    start_utf16: usize,
    selected_text: String,
    revision_id: Option<String>,
    reconstructed_text: Option<String>,
    translation: String,
    target: String,
    analysis: Option<Box<crate::LemmaSnapshot>>,
    expression: Option<Box<crate::ExpressionSnapshot>>,
    sense: Option<Box<crate::SenseSnapshot>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinguisticAnalysis {
    revision_id: String,
    sentence_id: String,
    language: String,
    package: Option<KnowledgePackage>,
    words: Vec<AnalyzedWord>,
    spacing_suggestions: Vec<SpacingSuggestion>,
    expressions: Vec<ExpressionMatch>,
    syntax_hints: Vec<&'static str>,
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

pub(crate) struct BoundaryAnalysis {
    text: String,
    visible_start: usize,
    projected: BTreeMap<usize, (usize, std::ops::Range<usize>)>,
    original: BTreeMap<usize, usize>,
}

impl BoundaryAnalysis {
    fn read(
        store: &Store, id: &str, page: u16, sentences: &[Sentence], sentence: &Sentence,
    ) -> Result<Option<Self>> {
        use crate::pdf_detached_pages::word_break;
        let (previous, next) = word_break::neighboring_fragments(
            store, id, page, None, sentences.first().filter(|first| first.id == sentence.id),
            sentences.last().filter(|last| last.id == sentence.id),
        )?;
        Ok(Self::from_fragments(sentence, previous.as_deref(), next.as_deref()))
    }

    pub(crate) fn canonical(store: &Store, document: &Document, sentence: &Sentence) -> Result<Option<Self>> {
        let (previous, next) = crate::pdf_detached_pages::word_break::canonical_fragments(store, document, sentence)?;
        Ok(Self::from_fragments(sentence, previous.as_deref(), next.as_deref()))
    }

    fn from_fragments(sentence: &Sentence, previous: Option<&str>, next: Option<&str>) -> Option<Self> {
        let joined = crate::pdf_detached_pages::word_break::join_fragments(&sentence.text, previous, next)?;
        let mut projected = BTreeMap::new();
        let mut original = BTreeMap::new();
        let mut joined_words = word_parts(&joined.text).peekable();
        for (byte, offset, _) in word_parts(&sentence.text) {
            let position = joined.center.start + byte;
            while joined_words.peek().is_some_and(|&(start, _, word)| start + word.len() <= position) {
                joined_words.next();
            }
            if let Some(&(start, projected_offset, word)) = joined_words.peek().filter(|&&(start, _, _)| start <= position) {
                projected.insert(offset, (projected_offset, start..start + word.len()));
                original.insert(projected_offset, offset);
            }
        }
        drop(joined_words);
        Some(Self { text: joined.text, visible_start: joined.center.start, projected, original })
    }

    pub(crate) fn lookup_word(
        &self, store: &Store, language: &str, offset: usize,
    ) -> Result<Option<WordLookup>> {
        let Some((start, word)) = self.project_word(offset) else { return Ok(None); };
        let context = WordContext::new(self.text.clone(), start);
        let mut lookup = store.lookup_word(language, word, Some(&context))?;
        if let Some(lookup) = &mut lookup {
            self.localize_lookup(lookup);
        }
        Ok(lookup)
    }

    fn project_word(&self, offset: usize) -> Option<(usize, &str)> {
        self.projected.get(&offset).map(|(offset, span)| (*offset, &self.text[span.clone()]))
    }

    fn original_offset(&self, offset: usize) -> Option<usize> {
        self.original.get(&offset).copied()
    }

    fn localize_lookup(&self, lookup: &mut WordLookup) {
        lookup.expressions.retain_mut(|expression| {
            let Some(starts) = expression.part_starts_utf16.iter()
                .map(|&offset| self.original_offset(offset)).collect::<Option<Vec<_>>>()
            else { return false; };
            expression.part_starts_utf16 = starts;
            true
        });
        lookup.syntax_evidence.retain_mut(|edge| {
            let Some(offset) = self.original_offset(edge.other_start_utf16) else { return false; };
            edge.other_start_utf16 = offset;
            true
        });
    }
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
        let boundary = BoundaryAnalysis::canonical(self, &document, sentence)?;
        self.analyze_scope(
            &document.language, sentence, expected_revision, &scope, versioned_index(&index)?, boundary.as_ref(),
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
        let boundary = BoundaryAnalysis::canonical(self, &document, sentence)?;
        self.cached_scope(
            (&document.id, language), sentence, expected_revision, &scope, versioned_index(&index)?, lexical, boundary.as_ref(),
        )
    }

    pub fn cached_detached_pdf_analysis(
        &self, document_id: &str, page: u16, request: DetachedPageAnalysis,
    ) -> Result<Value> {
        let cached = self.detached_pdf_sentences(document_id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let sentence_at = |id: &str| {
            cached.sentences.iter().find(|sentence| sentence.id == id)
                .ok_or("Речення сторінки PDF змінилося.")
        };
        let (sentence, scope) = match request {
            DetachedPageAnalysis::Sentence { sentence_id, start } =>
                (sentence_at(&sentence_id)?, AnalysisScope::Sentence { start }),
            DetachedPageAnalysis::Selection { start_utf16, text } =>
                resolve_text_selection(&cached.sentences, start_utf16, &text)?,
            DetachedPageAnalysis::Expression { sentence_id, revision_id, member_starts_utf16 } => {
                if cached.revision != revision_id {
                    return Err("Текст сторінки PDF змінився. Виберіть вираз знову.".into());
                }
                (sentence_at(&sentence_id)?, AnalysisScope::Expression { member_starts_utf16 })
            }
        };
        let index = self.open_knowledge_index(&cached.language)?;
        let lexical = lexicon::index_identity(&self.knowledge_directory, &cached.language);
        let boundary = BoundaryAnalysis::read(self, document_id, page, &cached.sentences, sentence)?;
        self.cached_scope(
            (document_id, &cached.language), sentence, &cached.revision, &scope,
            versioned_index(&index)?, lexical, boundary.as_ref(),
        )
    }

    pub fn lookup_detached_pdf_word(
        &self, document_id: &str, page: u16, start_utf16: usize, selected: &str,
    ) -> Result<Option<WordLookup>> {
        let cached = self.detached_pdf_sentences(document_id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        let (sentence, scope) = resolve_text_selection(&cached.sentences, start_utf16, selected)?;
        let boundary = BoundaryAnalysis::read(self, document_id, page, &cached.sentences, sentence)?;
        let (word, offset) = match scope {
            AnalysisScope::Word { start_utf16 } => {
                if let Some(boundary) = &boundary {
                    if boundary.project_word(start_utf16).is_some() {
                        let mut lookup = boundary.lookup_word(self, &cached.language, start_utf16)?;
                        if let Some(lookup) = &mut lookup {
                            lookup.bind_expression_tokens(|start| token_id(&cached.revision, &sentence.id, start));
                        }
                        return Ok(lookup);
                    }
                }
                let (_, _, word) = word_parts(&sentence.text).find(|(_, offset, _)| *offset == start_utf16)
                    .ok_or("Слово не знайдено у виділенні PDF.")?;
                (word, start_utf16)
            }
            AnalysisScope::Expression { .. } => {
                let trimmed = selected.trim_start();
                let leading = selected[..selected.len() - trimmed.len()].encode_utf16().count();
                (trimmed.trim_end(), start_utf16 + leading - sentence.start_utf16)
            }
            AnalysisScope::Sentence { .. } => return Ok(None),
        };
        let layout = crate::document_layout::detached_pdf_document(
            cached.text, &cached.language, self.detached_pdf_ocr(document_id, page)?.map(|ocr| ocr.lines),
        );
        let mask = crate::document_layout::mask_pdf_non_text;
        let spelling = crate::ocr::byte_at_utf16(&sentence.text, offset)
            .and_then(|offset| mask(&layout, word, sentence.start_utf8 + offset));
        let word = spelling.as_deref().unwrap_or(word).trim_end();
        let context = WordContext {
            sentence: mask(&layout, &sentence.text, sentence.start_utf8).unwrap_or_else(|| sentence.text.clone()),
            start_utf16: offset,
        };
        let mut lookup = self.lookup_word(&cached.language, word, Some(&context))?;
        if let Some(lookup) = &mut lookup {
            lookup.bind_expression_tokens(|start| token_id(&cached.revision, &sentence.id, start));
        }
        Ok(lookup)
    }

    pub fn save_detached_pdf_word(
        &mut self, document_id: &str, page: u16, input: DetachedVocabularyInput,
    ) -> Result<crate::Vocabulary> {
        let cached = self.detached_pdf_sentences(document_id, page)?
            .ok_or("Текст сторінки PDF недоступний.")?;
        if input.revision_id.as_deref().is_some_and(|expected| expected != cached.revision) {
            return Err("Текст сторінки PDF змінився. Виділіть фрагмент знову.".into());
        }
        let (sentence, _) = resolve_text_selection(&cached.sentences, input.start_utf16, &input.selected_text)?;
        let projected = crate::pdf_detached_pages::word_break::project_selection(
            self, (document_id, page, &cached.language), &cached.sentences, sentence, input.start_utf16, &input.selected_text,
        )?;
        if input.reconstructed_text.as_deref().is_some_and(|expected| projected.text != expected) {
            return Err("Текст сусідньої сторінки змінився. Перекладіть виділення знову.".into());
        }
        let word = projected.text.into_owned();
        let anchor = crate::dictionary_transfer::DictionarySentenceAnchor {
            sentence_id: sentence.id.clone(),
            revision_id: cached.revision,
            page: Some(page),
            sentence_start_utf16: sentence.start_utf16,
            selection: crate::dictionary_transfer::DictionarySourceSelection::from_selection(input.start_utf16, &input.selected_text),
        };
        self.save_word_with_anchor(crate::Vocabulary {
            id: String::new(),
            word,
            translation: input.translation,
            language: cached.language,
            target_language: input.target,
            context: sentence.text.clone(),
            document_id: document_id.to_owned(),
            forward: Default::default(),
            reverse: Default::default(),
            analysis: input.analysis,
            expression: input.expression,
            sense: input.sense,
            accepted_translations: Vec::new(),
        }, Some(anchor))
    }

    /// Warm one sentence in the current reading window. A changed cursor or
    /// revision makes queued work obsolete.
    pub fn prefetch_linguistics(
        &self, document_id: &str, anchor_id: &str, expected_revision: &str, step: i8, page: Option<u16>,
    ) -> Result<()> {
        if !(-1..=1).contains(&step) {
            return Err("Фоновий аналіз доступний лише для сусідніх речень.".into());
        }
        let detached;
        let canonical = page.is_none().then(|| self.token_document(document_id)).transpose()?;
        let (language, sentences, position, revision) = if let Some(page) = page {
            if self.pdf_reading_page(document_id)? != Some(page) {
                return Ok(());
            }
            let Some(cached) = self.detached_pdf_sentences(document_id, page)? else {
                return Ok(());
            };
            detached = cached;
            let position = detached.saved.min(detached.sentences.len() - 1);
            (detached.language.as_str(), detached.sentences.as_slice(), position, Some(detached.revision.as_str()))
        } else if let Some(document) = &canonical {
            (document.language.as_str(), document.sentences.as_slice(), document.position, document.current_revision_id.as_deref())
        } else {
            return Ok(());
        };
        if revision != Some(expected_revision)
            || sentences.get(position).map(|item| item.id.as_str()) != Some(anchor_id)
        {
            return Ok(());
        }
        let Some(position) = position.checked_add_signed(isize::from(step)) else {
            return Ok(());
        };
        let Some(sentence) = sentences.get(position)
            .filter(|sentence| sentence.text.len() <= MAX_PREFETCH_SENTENCE_BYTES)
        else {
            return Ok(());
        };
        let index = self.open_knowledge_index(language)?;
        let lookup_index = versioned_index(&index)?;
        let lexical = lexicon::index_identity(&self.knowledge_directory, language);
        if lookup_index.is_some() || lexical.is_some() {
            let boundary = if let Some(page) = page {
                BoundaryAnalysis::read(self, document_id, page, sentences, sentence)?
            } else if let Some(document) = &canonical {
                BoundaryAnalysis::canonical(self, document, sentence)?
            } else {
                None
            };
            self.cached_scope(
                (document_id, language), sentence, expected_revision, &AnalysisScope::Sentence { start: 0 },
                lookup_index, lexical, boundary.as_ref(),
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
        boundary: Option<&BoundaryAnalysis>,
    ) -> Result<Value> {
        let (document_id, language) = source;
        let grammar = lookup_index.map(|(pack, _, version)| (pack.sha256, version));
        let version = match scope {
            AnalysisScope::Sentence { .. } => SENTENCE_ANNOTATION_VERSION,
            _ => SELECTION_ANNOTATION_VERSION,
        };
        let mut address = serde_json::to_vec(&(
            version, revision, &sentence.id, language, grammar, lexical, scope,
        ))
        .map_err(|error| error.to_string())?;
        if let Some(boundary) = boundary {
            address.extend_from_slice(&(boundary.visible_start as u64).to_le_bytes());
            address.extend_from_slice(boundary.text.as_bytes());
        }
        let key = Sha256::digest(address);
        if let Some(annotation) = annotation_cache::read(&self.connection, document_id, key.as_ref()) {
            return Ok(annotation);
        }
        let analysis =
            self.analyze_scope(language, sentence, revision, scope, lookup_index, boundary)?;
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
        boundary: Option<&BoundaryAnalysis>,
    ) -> Result<LinguisticAnalysis> {
        let (selected, next_start) = select_words(&sentence.text, scope)?;
        let sentence_id = sentence.id.as_str();
        let package = PACKS.iter().find(|pack| pack.language == language).map(|pack| {
            pack.describe(lookup_index.map_or(0, |(_, _, version)| version))
        });
        let id_at = |offset| token_id(revision, sentence_id, offset);
        let analysis_text = boundary.map_or(sentence.text.as_str(), |boundary| boundary.text.as_str());
        let analysis_id_at = |offset| {
            if let Some(boundary) = boundary {
                boundary.original_offset(offset).map_or_else(
                    || format!("{revision}:{sentence_id}:context:{offset}"),
                    id_at,
                )
            } else {
                id_at(offset)
            }
        };
        let syntax_parse = match (scope, lookup_index) {
            (AnalysisScope::Sentence { start: 0 } | AnalysisScope::Word { .. } | AnalysisScope::Expression { .. }, Some((_, db, version)))
                if version >= 5 =>
            {
                let mut parse = syntax_parse::analyze(db, analysis_text, analysis_id_at)?;
                if boundary.is_some() || !matches!(scope, AnalysisScope::Sentence { .. }) {
                    let selected_ids: Vec<_> = selected.iter().map(|&(_, offset, _)| id_at(offset)).collect();
                    parse.retain_scope(&selected_ids);
                }
                Some(parse)
            }
            _ => None,
        };
        let members = match scope {
            AnalysisScope::Expression { member_starts_utf16 } => Some(member_starts_utf16),
            _ => None,
        };
        let spacing_suggestions = if let Some((_, db, _)) = lookup_index.filter(|(_, _, version)| *version >= 2) {
            let offsets: Vec<_> = selected.iter().map(|&(_, offset, _)| offset).collect();
            spacing_suggestions(db, sentence, &offsets)?
        } else {
            Vec::new()
        };
        let mut words = Vec::with_capacity(selected.len());
        let mut expressions = BTreeMap::new();
        let mut dependencies = BTreeMap::new();
        let mut syntax_hints = Vec::new();
        let mut context = WordContext::new(analysis_text.to_owned(), 0);
        for (byte, offset, visible_text) in selected {
            let (analysis_offset, text) = boundary.and_then(|boundary| boundary.project_word(offset))
                .unwrap_or((offset, visible_text));
            context.start_utf16 = analysis_offset;
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
                    if let Some(boundary) = boundary {
                        boundary.localize_lookup(&mut lookup);
                    }
                    lookup.bind_expression_tokens(id_at);
                    spelling_status = if !lookup.analyses.is_empty() || !lookup.meanings.is_empty() {
                        SpellingStatus::Attested
                    } else if !lookup.spelling_suggestions.is_empty() {
                        SpellingStatus::Suggested
                    } else if lookup.package.installed || lookup.lexical_package.as_ref().is_some_and(|pack| pack.installed) {
                        SpellingStatus::Unattested
                    } else {
                        SpellingStatus::Unavailable
                    };
                    morphology = lookup.analyses;
                    morphology.truncate(MAX_READINGS);
                    spelling_suggestions = lookup.spelling_suggestions;
                    if let Some(hint) = lookup.syntax_hint.filter(|hint| !syntax_hints.contains(hint)) {
                        syntax_hints.push(hint);
                    }
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
                    end_utf8: sentence.start_utf8 + byte + visible_text.len(),
                    start_utf16: sentence.start_utf16 + offset,
                    end_utf16: sentence.start_utf16 + offset + visible_text.encode_utf16().count(),
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
            spacing_suggestions,
            expressions: expressions.into_values().collect(),
            syntax_hints,
            syntax_candidates: dependencies.into_values().collect(),
            next_start,
            syntax_parse,
        })
    }
}

pub(crate) fn resolve_text_selection<'a>(
    sentences: &'a [Sentence], start_utf16: usize, selected: &str,
) -> Result<(&'a Sentence, AnalysisScope)> {
    const MAX_SELECTION_BYTES: usize = 4096;
    if selected.is_empty() || selected.len() > MAX_SELECTION_BYTES {
        return Err("Виділіть слово або вираз до восьми слів.".into());
    }
    let left_trimmed = selected.trim_start();
    let prefix = selected.len() - left_trimmed.len();
    let start_utf16 = start_utf16.checked_add(selected[..prefix].encode_utf16().count())
        .ok_or("Виділення завелике.")?;
    let selected = left_trimmed.trim_end();
    let end_utf16 = start_utf16.checked_add(selected.encode_utf16().count())
        .ok_or("Виділення завелике.")?;
    let sentence = sentences.iter().find(|sentence| {
        sentence.start_utf16 <= start_utf16 && sentence.end_utf16 >= end_utf16
    }).ok_or("Виділення має бути в межах одного речення.")?;
    let start = start_utf16 - sentence.start_utf16;
    let end = end_utf16 - sentence.start_utf16;
    let (Some(first), Some(last)) = (
        crate::ocr::byte_at_utf16(&sentence.text, start),
        crate::ocr::byte_at_utf16(&sentence.text, end),
    ) else {
        return Err("Некоректна позиція виділення.".into());
    };
    if sentence.text.get(first..last) != Some(selected) {
        return Err("Виділений текст змінився.".into());
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

pub(crate) type WordPart<'a> = (usize, usize, &'a str);

pub(crate) fn select_words<'a>(
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
