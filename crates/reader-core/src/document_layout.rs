use crate::{Document, Result, MAX_IMPORT};
use pdf_extract::{MediaBox, OutputDev, OutputError, PlainTextOutput, Transform};
use quick_xml::{events::Event, name::ResolveResult, NsReader};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    io::{Cursor, Read},
    ops::Range,
    path::Path,
};

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
}

pub(crate) struct Extracted {
    pub text: String,
    pub blocks: Vec<DocumentBlock>,
    pub pdf_page_starts: Option<Vec<Option<usize>>>,
}

struct BlockRange {
    offset: Range<usize>,
    page: Option<usize>,
    heading: Option<u8>,
}

impl Document {
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
        "docx" => parse_docx(bytes)?,
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
                heading: None,
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
            })
        })
        .collect()
}

fn parse_docx(bytes: &[u8]) -> Result<Extracted> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "Некоректний DOCX")?;
    let member = archive
        .by_name("word/document.xml")
        .map_err(|_| "У DOCX немає текстового документа")?;
    if member.size() > MAX_IMPORT as u64 {
        return Err("Розпакований документ завеликий".into());
    }
    let mut xml = String::new();
    member
        .take(MAX_IMPORT as u64 + 1)
        .read_to_string(&mut xml)
        .map_err(|error| error.to_string())?;
    if xml.len() > MAX_IMPORT {
        return Err("Розпакований документ завеликий".into());
    }
    let mut reader = NsReader::from_str(&xml);
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut paragraphs = Vec::new();
    let mut in_text = false;
    let mut in_properties = false;
    let mut start = 0;
    loop {
        let decoder = reader.decoder();
        let (namespace, event) = reader
            .read_resolved_event()
            .map_err(|error| format!("Некоректний XML: {error}"))?;
        let word_namespace = matches!(namespace, ResolveResult::Bound(uri)
            if matches!(uri.as_ref(),
                b"http://schemas.openxmlformats.org/wordprocessingml/2006/main"
                | b"http://purl.oclc.org/ooxml/wordprocessingml/main"));
        let empty = matches!(event, Event::Empty(_));
        match event {
            Event::Start(ref event) | Event::Empty(ref event)
                if word_namespace || event.name().as_ref().starts_with(b"w:") =>
            {
                match event.local_name().as_ref() {
                    b"p" if !empty => {
                        if let Some(heading) = paragraphs.last().copied() {
                            ranges.push(BlockRange {
                                offset: start..text.len(),
                                page: None,
                                heading,
                            });
                            start = text.len();
                        }
                        paragraphs.push(None);
                    }
                    b"pPr" if !empty => in_properties = true,
                    b"t" if !empty => in_text = true,
                    b"tab" if empty => text.push('\t'),
                    b"br" if empty => text.push('\n'),
                    b"pStyle" if in_properties => {
                        if let Some(heading) = paragraphs.last_mut() {
                            let value = docx_attribute(event, decoder)?;
                            *heading = value.as_deref().and_then(|style| {
                                if style == "Title" {
                                    return Some(1);
                                }
                                style
                                    .strip_prefix("Heading")?
                                    .parse::<u8>()
                                    .ok()
                                    .filter(|level| (1..=9).contains(level))
                            });
                        }
                    }
                    b"outlineLvl" if in_properties => {
                        if let Some(heading) = paragraphs.last_mut() {
                            *heading = docx_attribute(event, decoder)?
                                .and_then(|value| value.parse::<u8>().ok())
                                .filter(|level| *level < 9)
                                .map(|level| level + 1);
                        }
                    }
                    _ => {}
                }
            }
            Event::End(event) if word_namespace || event.name().as_ref().starts_with(b"w:") => {
                match event.local_name().as_ref() {
                    b"p" => {
                        ranges.push(BlockRange {
                            offset: start..text.len(),
                            page: None,
                            heading: paragraphs.pop().flatten(),
                        });
                        text.push('\n');
                        start = text.len();
                    }
                    b"pPr" => in_properties = false,
                    b"t" => in_text = false,
                    _ => {}
                }
            }
            Event::Text(event) if in_text => {
                text.push_str(&event.decode().map_err(|error| error.to_string())?)
            }
            Event::GeneralRef(event) if in_text => {
                let reference = event.decode().map_err(|error| error.to_string())?;
                if let Some(value) = quick_xml::escape::resolve_predefined_entity(&reference) {
                    text.push_str(value);
                } else if let Some(ch) = reference
                    .strip_prefix("#x")
                    .and_then(|value| u32::from_str_radix(value, 16).ok())
                    .or_else(|| {
                        reference
                            .strip_prefix('#')
                            .and_then(|value| value.parse::<u32>().ok())
                    })
                    .and_then(char::from_u32)
                {
                    text.push(ch);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        if text.len() > MAX_IMPORT {
            return Err("Витягнутий текст завеликий".into());
        }
    }
    if start < text.len() {
        ranges.push(BlockRange {
            offset: start..text.len(),
            page: None,
            heading: None,
        });
    }
    let blocks = materialize_blocks(&text, ranges);
    Ok(Extracted {
        text,
        blocks,
        pdf_page_starts: None,
    })
}

fn docx_attribute(
    event: &quick_xml::events::BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<Option<String>> {
    for attribute in event.attributes() {
        let attribute = attribute.map_err(|error| error.to_string())?;
        if attribute.key.local_name().as_ref() == b"val" {
            return attribute
                .decode_and_unescape_value(decoder)
                .map(|value| Some(value.into_owned()))
                .map_err(|error| error.to_string());
        }
    }
    Ok(None)
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

fn parse_pdf(bytes: &[u8]) -> Result<Extracted> {
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
    let boundaries = output.boundaries;
    let text = text.into_inner();
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
                heading: None,
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
