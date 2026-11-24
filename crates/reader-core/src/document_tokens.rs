use crate::{knowledge, ocr::OcrSpan, pdf_reading_position, Document, Result, Sentence, Store};
use rusqlite::Connection;
use serde::Serialize;
use unicode_segmentation::UnicodeSegmentation;

pub const DEFAULT_TOKEN_PAGE_SIZE: usize = 128;
pub const MAX_TOKEN_PAGE_SIZE: usize = 256;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentenceTokenPage {
    pub revision_id: String,
    pub sentence_id: String,
    pub tokens: Vec<SentenceToken>,
    pub next_start: Option<usize>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SentenceToken {
    pub id: String,
    pub ordinal: usize,
    pub text: String,
    /// Offsets in the complete canonical document.
    pub span: OcrSpan,
    pub sentence_start_utf16: usize,
    pub sentence_start_grapheme: usize,
    pub sentence_end_grapheme: usize,
    pub ocr_anchors: Vec<TokenOcrAnchor>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenOcrAnchor {
    pub page: u16,
    pub span: OcrSpan,
    pub recognized_text: String,
    pub raw_line_start_utf16: Option<usize>,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub line_confidence: Option<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingCursor {
    pub revision_id: String,
    pub sentence_id: String,
    pub position: usize,
    pub token_id: Option<String>,
    pub sentence_start_utf16: Option<usize>,
    pub token_length_utf16: Option<usize>,
}

impl Store {
    pub fn reading_cursor(&self, id: &str) -> Result<Option<ReadingCursor>> {
        let document = self.token_document(id)?;
        let Some(sentence) = document.sentences.get(document.position) else {
            return Ok(None);
        };
        let revision = revision_id(&document)?;
        Ok(Some(reading_cursor(
            revision,
            sentence,
            document.position,
            validated_cursor_offset(&document),
        )))
    }

    pub fn set_reading_cursor(
        &mut self,
        id: &str,
        sentence_id: &str,
        expected_revision: &str,
        start_utf16: Option<usize>,
    ) -> Result<ReadingCursor> {
        self.set_reading_cursor_for_page(id, sentence_id, expected_revision, start_utf16, None)
    }

    pub fn set_reading_cursor_for_page(
        &mut self,
        id: &str,
        sentence_id: &str,
        expected_revision: &str,
        start_utf16: Option<usize>,
        page: Option<u16>,
    ) -> Result<ReadingCursor> {
        let mut cursor = None;
        self.edit_document(id, |document, tx| {
            let revision = revision_id(document)?;
            if revision != expected_revision {
                return Err("Текст документа змінився. Оновіть позицію читання.".into());
            }
            let position = document
                .sentences
                .iter()
                .position(|sentence| sentence.id == sentence_id)
                .ok_or("Речення не знайдено")?;
            let sentence = &document.sentences[position];
            if start_utf16.is_some_and(|offset| word_at(&sentence.text, offset).is_none()) {
                return Err("Токен речення не знайдено".into());
            }
            cursor = Some(reading_cursor(revision, sentence, position, start_utf16));
            apply_cursor(document, tx, position, start_utf16, page)
        })?;
        cursor.ok_or_else(|| "Не вдалося оновити позицію читання".into())
    }

    pub fn anchor_reading_selection(
        &mut self, id: &str, expected_revision: &str, start_utf16: usize, length_utf16: usize,
    ) -> Result<Option<ReadingCursor>> {
        let end = start_utf16.checked_add(length_utf16)
            .filter(|&end| end > start_utf16)
            .ok_or("Некоректне виділення тексту")?;
        let mut cursor = None;
        self.edit_document(id, |document, tx| {
            let revision = revision_id(document)?;
            if revision != expected_revision {
                return Err("Текст документа змінився. Оновіть виділення.".into());
            }
            if document.sentences.last().is_none_or(|sentence| end > sentence.end_utf16)
                && end > document.text.encode_utf16().count()
            {
                return Err("Некоректне виділення тексту".into());
            }
            let position = document.sentences.partition_point(|sentence| sentence.end_utf16 <= start_utf16);
            let Some(sentence) = document.sentences.get(position).filter(|sentence| sentence.start_utf16 < end) else {
                return Ok(false);
            };
            let start = start_utf16.saturating_sub(sentence.start_utf16);
            let end = end - sentence.start_utf16;
            let offset = word_parts(&sentence.text).take_while(|(_, offset, _)| *offset < end)
                .find_map(|(_, offset, word)| (offset + word.encode_utf16().count() > start).then_some(offset));
            // The selection can be punctuation or part of a word in a continuing
            // sentence. Its physical page may differ from the start of the token.
            let absolute = start_utf16.max(sentence.start_utf16);
            let selected_page = pdf_reading_position::page_for_offset(document, absolute)
                .and_then(|page| pdf_reading_position::page_bounds(document, page).map(|bounds| (page, bounds)))
                .filter(|&(_, (left, right))| left <= absolute && absolute < right);
            let offset = offset.filter(|offset| selected_page.is_none_or(|(_, (left, right))| {
                let token = sentence.start_utf16 + offset;
                left <= token && token < right
            }));
            cursor = Some(reading_cursor(revision, sentence, position, offset));
            apply_cursor(document, tx, position, offset, selected_page.map(|(page, _)| page))
        })?;
        Ok(cursor)
    }

    pub fn sentence_tokens(
        &self,
        id: &str,
        sentence_id: &str,
        start: usize,
        limit: usize,
    ) -> Result<SentenceTokenPage> {
        if !(1..=MAX_TOKEN_PAGE_SIZE).contains(&limit) {
            return Err("Некоректний розмір сторінки токенів".into());
        }
        let document = self.token_document(id)?;
        let sentence = find_sentence(&document, sentence_id)?;
        let revision = revision_id(&document)?;
        let anchors = sentence_anchors(&document, sentence);
        let mut tokens = Vec::with_capacity(limit);
        let mut graphemes = sentence.text.grapheme_indices(true).enumerate().peekable();
        let mut grapheme_index = 0;
        let mut next_start = None;
        let mut first_anchor = 0;

        for (ordinal, (byte, utf16, word)) in word_parts(&sentence.text).enumerate().skip(start) {
            if tokens.len() == limit {
                next_start = Some(ordinal);
                break;
            }
            while graphemes
                .peek()
                .is_some_and(|(_, (offset, _))| *offset <= byte)
            {
                grapheme_index = graphemes.next().expect("peeked grapheme").0;
            }
            let start_grapheme = grapheme_index;
            let end = byte + word.len();
            while graphemes
                .peek()
                .is_some_and(|(_, (offset, _))| *offset < end)
            {
                grapheme_index = graphemes.next().expect("peeked grapheme").0;
            }
            let span = OcrSpan {
                start_utf8: sentence.start_utf8 + byte,
                end_utf8: sentence.start_utf8 + end,
                start_utf16: sentence.start_utf16 + utf16,
                end_utf16: sentence.start_utf16 + utf16 + word.encode_utf16().count(),
            };
            while anchors
                .get(first_anchor)
                .is_some_and(|anchor| anchor.span.end_utf8 <= span.start_utf8)
            {
                first_anchor += 1;
            }
            let ocr_anchors = anchors[first_anchor..]
                .iter()
                .take_while(|anchor| anchor.span.start_utf8 < span.end_utf8)
                .filter(|anchor| overlaps(span.start_utf8, span.end_utf8, anchor.span))
                .cloned()
                .collect();
            tokens.push(SentenceToken {
                id: token_id(revision, sentence_id, utf16),
                ordinal,
                text: word.to_owned(),
                span,
                sentence_start_utf16: utf16,
                sentence_start_grapheme: start_grapheme,
                sentence_end_grapheme: grapheme_index + 1,
                ocr_anchors,
            });
        }
        Ok(SentenceTokenPage {
            revision_id: revision.to_owned(),
            sentence_id: sentence_id.to_owned(),
            tokens,
            next_start,
        })
    }

    pub fn lookup_sentence_token(
        &self,
        id: &str,
        sentence_id: &str,
        expected_revision: &str,
        start_utf16: usize,
    ) -> Result<Option<knowledge::WordLookup>> {
        let document = self.token_document(id)?;
        let revision = revision_id(&document)?;
        if revision != expected_revision {
            return Err("Текст документа змінився. Оновіть токени речення.".into());
        }
        let sentence = find_sentence(&document, sentence_id)?;
        let word = word_at(&sentence.text, start_utf16)
            .ok_or("Токен речення не знайдено")?;
        let mut lookup = if let Some(boundary) = knowledge::BoundaryAnalysis::canonical(self, &document, sentence)? {
            boundary.lookup_word(self, &document.language, start_utf16)?
        } else {
            let context = knowledge::WordContext::new(sentence.text.clone(), start_utf16);
            self.lookup_word(&document.language, word, Some(&context))?
        };
        if let Some(lookup) = &mut lookup {
            lookup.bind_expression_tokens(|offset| token_id(revision, sentence_id, offset));
        }
        Ok(lookup)
    }

    pub(crate) fn token_document(&self, id: &str) -> Result<Document> {
        let body: String = self
            .connection
            .query_row("SELECT body FROM documents WHERE id=?1", [id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }
}

fn apply_cursor(document: &mut Document, db: &Connection, position: usize, offset: Option<usize>, page: Option<u16>) -> Result<bool> {
    let sentence = &document.sentences[position];
    let absolute = sentence.start_utf16 + offset.unwrap_or(0);
    if let Some(page) = page {
        let overlaps = pdf_reading_position::page_bounds(document, page)
            .is_some_and(|(start, end)| match offset {
                Some(_) => start <= absolute && absolute < end,
                None => sentence.start_utf16 < end && start < sentence.end_utf16,
            });
        if !overlaps {
            return Err("Позиція читання поза вибраною сторінкою PDF.".into());
        }
        pdf_reading_position::set_page(db, document, page, false)?;
    } else {
        pdf_reading_position::focus_for_offset(db, document, absolute)?;
    }
    let changed = document.position != position || document.reading_token_utf16 != offset;
    document.position = position;
    document.reading_token_utf16 = offset;
    Ok(changed)
}

pub(crate) fn revision_id(document: &Document) -> Result<&str> {
    document
        .current_revision_id
        .as_deref()
        .ok_or_else(|| "Ревізію документа не знайдено".into())
}

pub(crate) fn find_sentence<'a>(document: &'a Document, id: &str) -> Result<&'a Sentence> {
    document
        .sentences
        .iter()
        .find(|sentence| sentence.id == id)
        .ok_or_else(|| "Речення не знайдено".into())
}

pub(crate) fn token_id(revision: &str, sentence: &str, start_utf16: usize) -> String {
    format!("{revision}:{sentence}:{start_utf16}")
}

fn reading_cursor(
    revision: &str,
    sentence: &Sentence,
    position: usize,
    offset: Option<usize>,
) -> ReadingCursor {
    ReadingCursor {
        revision_id: revision.to_owned(),
        sentence_id: sentence.id.clone(),
        position,
        token_id: offset.map(|offset| token_id(revision, &sentence.id, offset)),
        sentence_start_utf16: offset,
        token_length_utf16: offset.and_then(|offset| word_at(&sentence.text, offset))
            .map(|word| word.encode_utf16().count()),
    }
}

pub(crate) fn validated_cursor_offset(document: &Document) -> Option<usize> {
    let offset = document.reading_token_utf16?;
    let sentence = document.sentences.get(document.position)?;
    word_at(&sentence.text, offset).map(|_| offset)
}

pub(crate) fn word_at(text: &str, start_utf16: usize) -> Option<&str> {
    word_parts(text)
        .take_while(|(_, offset, _)| *offset <= start_utf16)
        .find_map(|(_, offset, word)| (offset == start_utf16).then_some(word))
}

pub(crate) fn word_parts(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    text.split_word_bound_indices()
        .scan(0, |utf16, (byte, part)| {
            let start = *utf16;
            *utf16 += part.encode_utf16().count();
            Some((byte, start, part))
        })
        .filter(|(_, _, part)| part.chars().any(char::is_alphanumeric))
}

fn sentence_anchors(document: &Document, sentence: &Sentence) -> Vec<TokenOcrAnchor> {
    let mut anchors: Vec<_> = document
        .ocr_lines
        .iter()
        .flatten()
        .filter(|line| {
            line.source_span.is_some_and(|span| {
                overlaps(sentence.start_utf8, sentence.end_utf8, span)
            })
        })
        .flat_map(|line| {
            line.words.iter().filter_map(|word| {
                let span = word.source_span?;
                overlaps(sentence.start_utf8, sentence.end_utf8, span).then(|| TokenOcrAnchor {
                    page: line.page,
                    span,
                    recognized_text: word.text.clone(),
                    raw_line_start_utf16: word.raw_start_utf16,
                    x: word.x,
                    y: word.y,
                    width: word.width,
                    height: word.height,
                    line_confidence: line.confidence,
                })
            })
        })
        .collect();
    anchors.sort_unstable_by_key(|anchor| anchor.span.start_utf8);
    anchors
}

fn overlaps(start: usize, end: usize, span: OcrSpan) -> bool {
    start < span.end_utf8 && span.start_utf8 < end
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selecting_a_pdf_continuation_retains_the_clicked_page() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("reader.db");
        let mut store = Store::open(&database).unwrap();
        let source = root.path().join("book.pdf");
        std::fs::write(&source, b"%PDF-1.4\n").unwrap();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(),
            "book.pdf",
            &source,
            "fr",
            vec!["Le marin regarde".into(), "la mer. Il attend.".into(), "Le vent se lève.".into()],
            vec![],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        // Simulate a saved text revision with a sentence spanning two physical pages.
        store.edit_document(&document.id, |doc, _| {
            doc.sentences = crate::segment(&doc.text.replace("\n\n", "  "), &doc.id);
            for sentence in &mut doc.sentences {
                sentence.text = doc.text[sentence.start_utf8..sentence.end_utf8].to_owned();
            }
            Ok(true)
        }).unwrap();
        let document = store.token_document(&document.id).unwrap();
        let revision = revision_id(&document).unwrap();
        let sentence = &document.sentences[0];
        let second_page_start = document.pdf_page_starts.as_ref().unwrap()[1].unwrap();
        assert!(sentence.start_utf16 < second_page_start && second_page_start < sentence.end_utf16);
        store.set_reading_cursor(&document.id, &document.sentences[1].id, revision, None).unwrap();

        let selected = store.set_reading_cursor_for_page(&document.id, &sentence.id, revision, None, Some(1)).unwrap();
        assert_eq!(selected.sentence_id, sentence.id);
        assert_eq!(store.pdf_reading_page(&document.id).unwrap(), Some(1));
        assert!(store.set_reading_cursor_for_page(&document.id, &sentence.id, revision, None, Some(2)).is_err());
        assert_eq!(store.pdf_reading_page(&document.id).unwrap(), Some(1));

        // There is no word boundary for the period, but its page still wins.
        store.set_pdf_reading_page(&document.id, 0).unwrap();
        let anchored = store.anchor_reading_selection(&document.id, revision, sentence.end_utf16 - 1, 1)
            .unwrap().unwrap();
        assert_eq!(anchored.sentence_id, sentence.id);
        assert_eq!(anchored.sentence_start_utf16, None);
        assert_eq!(store.pdf_reading_page(&document.id).unwrap(), Some(1));

        drop(store);
        let store = Store::open(&database).unwrap();
        assert_eq!(store.reading_cursor(&document.id).unwrap().unwrap().sentence_id, sentence.id);
        assert_eq!(store.pdf_reading_page(&document.id).unwrap(), Some(1));
    }
}
