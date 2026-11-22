use crate::{knowledge, ocr::OcrSpan, Document, Result, Sentence, Store};
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

impl Store {
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
        let word = word_parts(&sentence.text)
            .take_while(|(_, offset, _)| *offset <= start_utf16)
            .find_map(|(_, offset, word)| (offset == start_utf16).then_some(word))
            .ok_or("Токен речення не знайдено")?;
        let context = knowledge::WordContext::new(sentence.text.clone(), start_utf16);
        let mut lookup = self.lookup_word(&document.language, word, Some(&context))?;
        if let Some(lookup) = &mut lookup {
            lookup.bind_expression_tokens(|offset| token_id(revision, sentence_id, offset));
        }
        Ok(lookup)
    }

    fn token_document(&self, id: &str) -> Result<Document> {
        let body: String = self
            .connection
            .query_row("SELECT body FROM documents WHERE id=?1", [id], |row| row.get(0))
            .map_err(|error| error.to_string())?;
        serde_json::from_str(&body).map_err(|error| error.to_string())
    }
}

fn revision_id(document: &Document) -> Result<&str> {
    document
        .current_revision_id
        .as_deref()
        .ok_or_else(|| "Ревізію документа не знайдено".into())
}

fn find_sentence<'a>(document: &'a Document, id: &str) -> Result<&'a Sentence> {
    document
        .sentences
        .iter()
        .find(|sentence| sentence.id == id)
        .ok_or_else(|| "Речення не знайдено".into())
}

fn token_id(revision: &str, sentence: &str, start_utf16: usize) -> String {
    format!("{revision}:{sentence}:{start_utf16}")
}

fn word_parts(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
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
