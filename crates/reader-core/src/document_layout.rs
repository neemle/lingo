use crate::{Document, Result, MAX_IMPORT};
use pdf_extract::{MediaBox, OutputDev, OutputError, PlainTextOutput, Transform};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    ops::Range,
    path::Path,
};

mod docx;

pub(crate) use docx::DOCX_LAYOUT_VERSION;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    Paragraph,
    Heading,
}

/// A block points into the same canonical text and offset spaces as sentences.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentBlock {
    pub kind: BlockKind,
    pub start_utf8: usize,
    pub end_utf8: usize,
    pub start_utf16: usize,
    pub end_utf16: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading_level: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<DocxList>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_cell: Option<DocxTableCell>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocxList {
    pub id: u32,
    pub level: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocxTableCell {
    pub table: u32,
    pub row: u32,
    pub column: u32,
    pub column_span: u32,
}

pub(crate) struct Extracted {
    pub text: String,
    pub blocks: Vec<DocumentBlock>,
    pub pdf_page_starts: Option<Vec<Option<usize>>>,
}

#[derive(Default)]
struct BlockRange {
    offset: Range<usize>,
    page: Option<usize>,
    heading: Option<u8>,
    list: Option<DocxList>,
    table_cell: Option<DocxTableCell>,
}

impl Document {
    pub(crate) fn paragraph_for_sentence(&self, sentence: &crate::Sentence) -> Range<usize> {
        // French OCR can leave a closing quote from the previous block at the
        // start of a sentence. Anchor the paragraph to the sentence itself.
        let first = sentence.text.find(char::is_alphanumeric).unwrap_or(0);
        let last = sentence.text.rfind(char::is_alphanumeric).unwrap_or(first);
        let start = self.paragraph_utf8_range(sentence.start_utf8 + first).start;
        let end = self.paragraph_utf8_range(sentence.start_utf8 + last).end;
        start..end.max(sentence.end_utf8)
    }

    /// Return the enclosing paragraph or heading in canonical UTF-8 offsets.
    pub fn paragraph_utf8_range(&self, source_byte_offset: usize) -> Range<usize> {
        let offset = source_byte_offset.min(self.text.len());
        let previous = self
            .blocks
            .partition_point(|block| block.start_utf8 <= offset);
        if let Some(block) = previous
            .checked_sub(1)
            .and_then(|index| self.blocks.get(index))
        {
            if offset < block.end_utf8 && self.text.get(block.start_utf8..block.end_utf8).is_some()
            {
                return block.start_utf8..block.end_utf8;
            }
        }
        // Older documents retain their text and sentence IDs. Their native
        // structures can also be recovered from the original on reimport.
        let pages = self.pdf_page_starts.as_deref();
        let mut found = None;
        visit_page_ranges(&self.text, pages, |range, _| {
            if range.contains(&offset) {
                found = paragraph_ranges(&self.text, range, self.format == "docx")
                    .find(|paragraph| paragraph.contains(&offset));
            }
        });
        found.unwrap_or(0..self.text.len())
    }
}

pub(crate) fn parse(name: &str, bytes: &[u8]) -> Result<Extracted> {
    if bytes.len() > MAX_IMPORT {
        return Err("Файл завеликий. Максимум — 20 МБ.".into());
    }
    let extension = Path::new(name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let parsed = match extension.as_str() {
        "txt" => {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| "TXT має бути в кодуванні UTF-8")?
                .trim_start_matches('\u{feff}')
                .to_owned();
            from_transcript(text, None)
        }
        "docx" => docx::parse(bytes)?,
        "pdf" => parse_pdf(bytes)?,
        _ => return Err("Формат документа не підтримується.".into()),
    };
    if parsed.text.trim().is_empty() {
        return Err(
            "Тексту не знайдено. Для скану PDF скористайтеся дією «Розпізнати PDF».".into(),
        );
    }
    Ok(parsed)
}

pub(crate) fn from_transcript(text: String, page_starts: Option<Vec<Option<usize>>>) -> Extracted {
    let mut ranges = Vec::new();
    visit_page_ranges(&text, page_starts.as_deref(), |range, page| {
        ranges.extend(
            paragraph_ranges(&text, range, false).map(|offset| BlockRange {
                offset,
                page,
                ..BlockRange::default()
            }),
        );
    });
    let blocks = materialize_blocks(&text, ranges);
    Extracted {
        text,
        blocks,
        pdf_page_starts: page_starts,
    }
}

/// Recover paragraph ranges from OCR geometry without changing the recognized text or its offsets.
pub(crate) fn reflow_ocr_paragraphs(document: &mut Document) {
    let Some(lines) = document.ocr_lines.as_deref().filter(|lines| document.format == "pdf" && !lines.is_empty())
    else { return; };
    let mut starts = std::collections::BTreeSet::new();
    let mut ranges = Vec::new();
    for page in lines.chunk_by(|left, right| left.page == right.page) {
        if page.iter().any(|line| line.source_span.is_none()) { continue; }
        let page_number = usize::from(page[0].page);
        starts.insert(page_number);
        let mut heights: Vec<f64> = page.iter().map(|line| line.height).collect();
        heights.sort_by(f64::total_cmp);
        let height = heights[heights.len() / 2];
        let mut left_edges: Vec<f64> = page.iter().filter(|line| line.width >= 0.2).map(|line| line.x).collect();
        left_edges.sort_by(f64::total_cmp);
        let body_left = left_edges.get(left_edges.len() / 4).copied().unwrap_or(page[0].x);
        let mut begin = page[0].source_span.expect("verified OCR span").start_utf8;
        for pair in page.windows(2) {
            let [previous, next] = pair else { continue; };
            let close_lines = next.y - previous.y <= (height * 1.9).max(0.045);
            let indented = next.x >= body_left + 0.027 && next.x >= previous.x + 0.025 &&
                previous.reading_text().trim_end_matches(['»', '”', '"', ')']).trim_end()
                    .ends_with(['.', '!', '?', ':', '…']);
            let footer = next.y >= 0.90 && next.reading_text().chars().all(|ch| ch.is_ascii_digit() || ch.is_whitespace() || ch == '-');
            if close_lines && !indented && !footer { continue; }
            let end = previous.source_span.expect("verified OCR span").end_utf8;
            ranges.push(BlockRange { offset: begin..end, page: Some(page_number), ..BlockRange::default() });
            begin = next.source_span.expect("verified OCR span").start_utf8;
        }
        let end = page.last().and_then(|line| line.source_span).expect("verified OCR span").end_utf8;
        ranges.push(BlockRange { offset: begin..end, page: Some(page_number), ..BlockRange::default() });
    }

    ranges.retain(|range| document.text.get(range.offset.clone()).is_some());
    let blocks = materialize_blocks(&document.text, ranges);
    document.blocks.retain(|block| block.page.is_none_or(|page| !starts.contains(&page)));
    document.blocks.extend(blocks);
    document.blocks.sort_by_key(|block| block.start_utf8);
}

pub(crate) fn from_ocr(
    text: String,
    lines: &[crate::ocr::OcrLine],
    pdf_pages: Option<u16>,
) -> Extracted {
    let mut page_starts = vec![None; usize::from(pdf_pages.unwrap_or(1))];
    let mut utf16 = 0;
    let mut previous = None;
    for line in lines {
        if let Some(page) = previous {
            utf16 += if page == line.page { 1 } else { 2 };
        }
        if previous != Some(line.page) {
            page_starts[usize::from(line.page)] = Some(utf16);
        }
        utf16 += line.reading_text().encode_utf16().count();
        previous = Some(line.page);
    }
    let mut parsed = from_transcript(text, Some(page_starts));
    // OCR-only PDF restore and upgrade use the presence of this field to
    // distinguish mixed extraction. Page indices remain present on blocks.
    parsed.pdf_page_starts = None;
    parsed
}

fn visit_page_ranges(
    text: &str,
    starts: Option<&[Option<usize>]>,
    mut visit: impl FnMut(Range<usize>, Option<usize>),
) {
    let Some(starts) = starts else {
        visit(0..text.len(), None);
        return;
    };
    let offsets = starts
        .iter()
        .enumerate()
        .filter_map(|(page, offset)| offset.map(|offset| (page, offset)));
    let mut chars = text.char_indices();
    let mut bytes = 0;
    let mut utf16 = 0;
    let mut current = None;
    for (page, offset) in offsets {
        while utf16 < offset {
            let Some((byte, ch)) = chars.next() else {
                break;
            };
            bytes = byte + ch.len_utf8();
            utf16 += ch.len_utf16();
        }
        if utf16 != offset {
            continue;
        }
        if let Some((prior, start)) = current {
            if bytes > start {
                visit(start..bytes, Some(prior));
            }
        }
        current = Some((page, bytes));
    }
    if let Some((page, start)) = current {
        visit(start..text.len(), Some(page));
    }
}

fn paragraph_ranges(
    text: &str,
    range: Range<usize>,
    each_line: bool,
) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut from = range.start;
    let mut next = from;
    let mut lines = text[range.clone()].split_inclusive('\n');
    std::iter::from_fn(move || loop {
        let end = match lines.next() {
            Some(line) => {
                let start = next;
                next += line.len();
                if !each_line && !line.trim().is_empty() {
                    continue;
                }
                if each_line {
                    next
                } else {
                    start
                }
            }
            None if from < range.end => range.end,
            None => return None,
        };
        let start = from;
        from = next;
        if let Some(range) = trimmed_range(text, start..end) {
            return Some(range);
        }
    })
}

fn trimmed_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let span = &text[range.clone()];
    let trimmed = span.trim_start();
    let start = range.start + span.len() - trimmed.len();
    let end = start + trimmed.trim_end().len();
    (start < end).then_some(start..end)
}

fn materialize_blocks(text: &str, ranges: Vec<BlockRange>) -> Vec<DocumentBlock> {
    let mut previous = 0;
    let mut utf16 = 0;
    ranges
        .into_iter()
        .filter_map(|range| {
            let offset = trimmed_range(text, range.offset)?;
            let start_utf16 = utf16 + text[previous..offset.start].encode_utf16().count();
            utf16 = start_utf16 + text[offset.clone()].encode_utf16().count();
            previous = offset.end;
            Some(DocumentBlock {
                kind: if range.heading.is_some() {
                    BlockKind::Heading
                } else {
                    BlockKind::Paragraph
                },
                start_utf8: offset.start,
                end_utf8: offset.end,
                start_utf16,
                end_utf16: utf16,
                page: range.page,
                heading_level: range.heading,
                list: range.list,
                table_cell: range.table_cell,
            })
        })
        .collect()
}

type PdfBuffer = RefCell<String>;

struct PdfWriter<'a>(&'a PdfBuffer);
impl pdf_extract::ConvertToFmt for PdfWriter<'_> {
    type Writer = Self;
    fn convert(self) -> Self {
        self
    }
}
impl std::fmt::Write for PdfWriter<'_> {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        let mut text = self.0.borrow_mut();
        if text.len().saturating_add(value.len()) > MAX_IMPORT {
            return Err(std::fmt::Error);
        }
        text.push_str(value);
        Ok(())
    }
}

struct PageOutput<'a> {
    text: &'a PdfBuffer,
    writer: PlainTextOutput<PdfWriter<'a>>,
    boundaries: Vec<(usize, usize)>,
}

impl OutputDev for PageOutput<'_> {
    fn begin_page(
        &mut self,
        page: u32,
        bounds: &MediaBox,
        art: Option<(f64, f64, f64, f64)>,
    ) -> std::result::Result<(), OutputError> {
        self.boundaries
            .push((page as usize - 1, self.text.borrow().len()));
        self.writer.begin_page(page, bounds, art)
    }
    fn end_page(&mut self) -> std::result::Result<(), OutputError> {
        self.writer.end_page()
    }
    fn output_character(
        &mut self,
        transform: &Transform,
        width: f64,
        space: f64,
        size: f64,
        text: &str,
    ) -> std::result::Result<(), OutputError> {
        self.writer
            .output_character(transform, width, space, size, text)
    }
    fn begin_word(&mut self) -> std::result::Result<(), OutputError> {
        self.writer.begin_word()
    }
    fn end_word(&mut self) -> std::result::Result<(), OutputError> {
        self.writer.end_word()
    }
    fn end_line(&mut self) -> std::result::Result<(), OutputError> {
        self.writer.end_line()
    }
}

struct PdfText {
    text: String,
    boundaries: Vec<(usize, usize)>,
}

fn extract_pdf_text(bytes: &[u8]) -> Result<PdfText> {
    if !bytes.starts_with(b"%PDF-") {
        return Err("Некоректний PDF".into());
    }
    let pdf_error = |error: &dyn std::fmt::Display| format!("Не вдалося прочитати PDF: {error}");
    let mut pdf = pdf_extract::Document::load_mem(bytes).map_err(|error| pdf_error(&error))?;
    if pdf.is_encrypted() {
        pdf.decrypt("").map_err(|error| pdf_error(&error))?;
    }
    let text = PdfBuffer::default();
    let mut output = PageOutput {
        text: &text,
        writer: PlainTextOutput::new(PdfWriter(&text)),
        boundaries: Vec::new(),
    };
    pdf_extract::output_doc(&pdf, &mut output).map_err(|error| match error {
        OutputError::FormatError(_) => "Витягнутий текст PDF завеликий".into(),
        error => pdf_error(&error),
    })?;
    Ok(PdfText {
        boundaries: output.boundaries,
        text: text.into_inner(),
    })
}

pub(crate) fn pdf_text_pages(bytes: &[u8], expected: u16) -> Result<Vec<String>> {
    let PdfText { text, boundaries } = extract_pdf_text(bytes)?;
    if boundaries.len() != usize::from(expected) {
        return Err("Не вдалося звірити сторінки PDF з текстовим шаром".into());
    }
    Ok(boundaries
        .iter()
        .enumerate()
        .map(|(index, &(_, start))| {
            let end = boundaries
                .get(index + 1)
                .map_or(text.len(), |&(_, end)| end);
            text[start..end]
                .trim_matches(|c: char| c.is_whitespace() || c.is_control() || c == '\u{fffd}')
                .to_owned()
        })
        .collect())
}

fn parse_pdf(bytes: &[u8]) -> Result<Extracted> {
    let PdfText { text, boundaries } = extract_pdf_text(bytes)?;
    let mut page_starts = vec![None; boundaries.len()];
    let mut ranges = Vec::new();
    let mut previous = 0;
    let mut utf16 = 0;
    for (index, &(page, start)) in boundaries.iter().enumerate() {
        let end = boundaries
            .get(index + 1)
            .map_or(text.len(), |&(_, end)| end);
        let Some(span) = trimmed_range(&text, start..end) else {
            continue;
        };
        utf16 += text[previous..span.start].encode_utf16().count();
        if let Some(page_start) = page_starts.get_mut(page) {
            *page_start = Some(utf16);
        }
        previous = span.start;
        ranges.extend(
            paragraph_ranges(&text, span, false).map(|offset| BlockRange {
                offset,
                page: Some(page),
                ..BlockRange::default()
            }),
        );
    }
    let blocks = materialize_blocks(&text, ranges);
    Ok(Extracted {
        text,
        blocks,
        pdf_page_starts: Some(page_starts),
    })
}
