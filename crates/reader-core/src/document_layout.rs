use crate::{Document, Result, Sentence, MAX_IMPORT};
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub footnote: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub footnote_references: Vec<crate::ocr::OcrSpan>,
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
    footnote: bool,
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
        "txt" => from_transcript(decode_txt(bytes)?, None),
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

pub(crate) fn decode_txt(bytes: &[u8]) -> Result<String> {
    const INVALID: &str = "Некоректне кодування TXT. Підтримуються UTF-8, UTF-16 та UTF-32 з позначкою BOM для UTF-16/32.";
    match bytes {
        // Check UTF-32 first because its little-endian BOM starts with the UTF-16 BOM.
        [0xff, 0xfe, 0x00, 0x00, rest @ ..] | [0x00, 0x00, 0xfe, 0xff, rest @ ..] => {
            let (units, partial) = rest.as_chunks::<4>();
            if !partial.is_empty() {
                return Err(INVALID.into());
            }
            let decode = if bytes[0] == 0xff { u32::from_le_bytes } else { u32::from_be_bytes };
            units.iter().map(|&unit| char::from_u32(decode(unit)).ok_or_else(|| INVALID.to_owned())).collect()
        }
        [0xff, 0xfe, rest @ ..] | [0xfe, 0xff, rest @ ..] => {
            let (units, partial) = rest.as_chunks::<2>();
            if !partial.is_empty() {
                return Err(INVALID.into());
            }
            let decode = if bytes[0] == 0xff { u16::from_le_bytes } else { u16::from_be_bytes };
            char::decode_utf16(units.iter().copied().map(decode))
                .map(|character| character.map_err(|_| INVALID.to_owned()))
                .collect()
        }
        _ => std::str::from_utf8(bytes)
            .map(|text| text.trim_start_matches('\u{feff}').to_owned())
            .map_err(|_| INVALID.into()),
    }
}

pub(crate) fn from_transcript(text: String, page_starts: Option<Vec<Option<usize>>>) -> Extracted {
    let mut ranges = Vec::new();
    visit_page_ranges(&text, page_starts.as_deref(), |range, page| {
        ranges.extend(
            transcript_ranges(&text, range, page.is_some()).into_iter().map(|(offset, heading)| BlockRange {
                offset,
                page,
                heading,
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

fn transcript_ranges(text: &str, range: Range<usize>, pdf: bool) -> Vec<(Range<usize>, Option<u8>)> {
    if !pdf {
        return paragraph_ranges(text, range, false).map(|range| (range, None)).collect();
    }

    let page = &text[range.clone()];
    let mut widths: Vec<_> = page.lines().map(|line| line.trim().chars().count())
        .filter(|&width| width >= 30).collect();
    widths.sort_unstable();
    let typical_width = widths.get(widths.len() / 2).copied().unwrap_or(0);
    let mut result = Vec::new();
    let mut start = range.start;
    let mut offset = start;
    let mut previous = "";
    for line in page.split_inclusive('\n') {
        let content = line.trim();
        let mut tail = content;
        let heading = standalone_heading(content);
        let footnote = footnote_start(content) && (offset == start || ends_sentence(previous));
        let new_prose = content.starts_with(char::is_uppercase)
            && (previous.ends_with(['»', '”'])
                || ends_sentence(previous) && previous.chars().count() * 2 < typical_width);
        if content.is_empty() || starts_dialogue(content, previous) || new_prose || heading || footnote {
            if let Some(part) = trimmed_range(text, start..offset) {
                result.push((part, None));
            }
            start = offset;
        }
        if !heading {
            let leading = line.len() - line.trim_start().len();
            for index in inline_pdf_breaks(content) {
                let split = offset + leading + index;
                if let Some(part) = trimmed_range(text, start..split) {
                    result.push((part, None));
                }
                start = split;
                tail = &content[index..];
            }
        }
        offset += line.len();
        if heading {
            result.push((trimmed_range(text, start..offset).expect("nonempty heading"), Some(1)));
            start = offset;
        } else if content.is_empty() {
            start = offset;
        }
        previous = tail;
    }
    if let Some(part) = trimmed_range(text, start..range.end) {
        result.push((part, None));
    }
    result
}

fn inline_pdf_breaks(line: &str) -> impl Iterator<Item = usize> + '_ {
    let dialogue_line = starts_dialogue(line, "");
    line.char_indices().filter_map(move |(index, ch)| {
        let possible_marker = matches!(ch, '–' | '—' | '-' | '«' | '“' | '„' | '"')
            || ch.is_ascii_digit();
        if index == 0 || (!possible_marker && !ch.is_uppercase()) {
            return None;
        }
        let preceding = &line[..index];
        if !preceding.ends_with(char::is_whitespace) {
            return None;
        }
        let preceding = preceding.trim_end();
        let following = &line[index..];
        let sentence_end = ends_sentence(preceding);
        // A text layer can collapse both the opening speech and the following
        // narration into the physical line holding the quotation.
        if ch.is_uppercase() && preceding.ends_with(['»', '”', '›', '」', '』'])
            && (sentence_end || dialogue_line)
        {
            return Some(index);
        }
        // OCR can omit a turn's final punctuation. In an already marked
        // dialogue, a spaced long dash followed by a capital can start the next turn.
        if dialogue_line && matches!(ch, '—' | '–') &&
            following[ch.len_utf8()..].starts_with(char::is_whitespace) &&
            following[ch.len_utf8()..].trim_start().starts_with(char::is_uppercase)
        {
            return Some(index);
        }
        if !sentence_end { return None; }
        (possible_marker && (starts_dialogue(following, preceding) || footnote_start(following)))
            .then_some(index)
    })
}

fn standalone_heading(text: &str) -> bool {
    let letters = text.chars().filter(|ch| ch.is_alphabetic()).count();
    (4..=30).contains(&letters)
        && text.chars().all(|ch| ch.is_uppercase() || ch.is_ascii_digit() || ch.is_whitespace())
}

pub(crate) fn footnote_start(text: &str) -> bool {
    let remainder = text.trim_start_matches(|ch: char| ch.is_ascii_digit());
    if !(1..=2).contains(&(text.len() - remainder.len())) || !remainder.starts_with(char::is_whitespace) {
        return false;
    }
    glossary_definition(remainder.trim_start())
}

fn glossary_definition(text: &str) -> bool {
    text.split_once(':').is_some_and(|(term, gloss)| {
        (1..=3).contains(&term.split_whitespace().count())
            && term.trim_start().starts_with(char::is_alphabetic)
            && gloss.trim_start().starts_with(char::is_alphabetic)
    })
}

pub(crate) fn sentence_in_footnote(blocks: &[DocumentBlock], sentence: &crate::Sentence) -> bool {
    let offset = sentence.start_utf8 + sentence.text.find(char::is_alphanumeric).unwrap_or(0);
    let index = blocks.partition_point(|block| block.start_utf8 <= offset);
    index.checked_sub(1).and_then(|index| blocks.get(index))
        .is_some_and(|block| block.footnote && offset < block.end_utf8)
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['»', '”', '›', '」', '』', '"', ')']).trim_end()
        .ends_with(['.', '!', '?', ':', '…'])
}

fn starts_dialogue(line: &str, previous: &str) -> bool {
    if let Some(rest) = line.strip_prefix(['—', '–', '-']) {
        return (rest.starts_with(char::is_whitespace) || !line.starts_with('-'))
            && rest.trim_start().starts_with(char::is_alphabetic);
    }
    (previous.is_empty() || ends_sentence(previous))
        && line.strip_prefix(['«', '“', '„', '"'])
            .is_some_and(|rest| rest.trim_start().starts_with(char::is_alphabetic))
}

pub(crate) fn keep_pdf_sentence_break(previous: &str, next: &str) -> bool {
    standalone_heading(previous) || standalone_heading(next) || starts_dialogue(next, previous)
        || (ends_sentence(previous) && footnote_start(next))
}

pub(crate) fn detached_pdf_document(
    text: String,
    language: &str,
    ocr_lines: Option<Vec<crate::ocr::OcrLine>>,
) -> Document {
    let layout = from_transcript(text, Some(vec![Some(0)]));
    let mut document = Document {
        text: layout.text,
        blocks: layout.blocks,
        language: language.to_owned(),
        format: "pdf".into(),
        ocr_lines,
        ..Document::default()
    };
    // Cached OCR keeps the physical page number, but its spans are page-relative.
    if let Some(page) = document.ocr_lines.as_ref().and_then(|lines| lines.first()).map(|line| usize::from(line.page)) {
        for block in &mut document.blocks {
            block.page = Some(page);
        }
    }
    reflow_ocr_paragraphs(&mut document);
    document
}

/// Recover paragraph ranges from OCR geometry without changing the recognized text or its offsets.
pub(crate) fn reflow_ocr_paragraphs(document: &mut Document) {
    let pdf = document.format == "pdf";
    if !pdf && document.ocr_lines.is_none() { return; }
    if let Some(starts) = document.pdf_page_starts.as_deref() {
        let mut ranges = Vec::new();
        visit_page_ranges(&document.text, Some(starts), |range, page| {
            ranges.extend(transcript_ranges(&document.text, range, true).into_iter().map(|(offset, heading)| BlockRange {
                offset, page, heading, ..BlockRange::default()
            }));
        });
        document.blocks = materialize_blocks(&document.text, ranges);
    }
    let Some(lines) = document.ocr_lines.as_deref().filter(|lines| !lines.is_empty())
    else { return; };
    let mut starts = std::collections::BTreeSet::new();
    let mut ranges = Vec::new();
    for page in lines.chunk_by(|left, right| left.page == right.page) {
        if page.iter().any(|line| line.source_span.is_none()) { continue; }
        let page_number = usize::from(page[0].page);
        let mut heights: Vec<f64> = page.iter().filter(|line| !line.is_rotated()).map(|line| line.height).collect();
        heights.sort_by(f64::total_cmp);
        let height = heights.get(heights.len() / 2).copied().unwrap_or(0.0);
        let mut left_edges: Vec<f64> = page.iter().filter(|line| !line.is_rotated() && line.width >= 0.2)
            .map(|line| line.x).collect();
        left_edges.sort_by(f64::total_cmp);
        let body_left = left_edges.get(left_edges.len() / 4).copied().unwrap_or(page[0].x);
        let page_start = page[0].source_span.expect("verified OCR span").start_utf8;
        let page_end = page.last().and_then(|line| line.source_span).expect("verified OCR span").end_utf8;
        if document.text.get(page_start..page_end).is_none() { continue; }
        starts.insert(page_number);
        let mut geometry_breaks = Vec::new();
        let numbered_notes = numbered_ocr_footnotes(page);
        for note in &numbered_notes {
            geometry_breaks.extend([note.start, note.end]);
        }
        let mut unnumbered_notes = std::collections::BTreeSet::new();
        // OCR can omit the superscript numbers. A run of small definitions at the
        // bottom of a page is enough evidence to keep them out of the prose.
        for run in page.chunk_by(|left, right| {
            let definition = |line: &crate::ocr::OcrLine| !line.is_rotated()
                && line.reading_text().starts_with(char::is_uppercase)
                && glossary_definition(line.reading_text());
            definition(left) && definition(right)
        }) {
            if run.len() < 3 || run[0].y < 0.70 ||
                run.iter().map(|line| line.height).sum::<f64>() > height * run.len() as f64 * 1.03 {
                continue;
            }
            for line in run {
                let span = line.source_span.expect("verified OCR span");
                if document.text.is_char_boundary(span.start_utf8) && document.text.is_char_boundary(span.end_utf8) {
                    unnumbered_notes.insert(span.start_utf8);
                    geometry_breaks.extend([span.start_utf8, span.end_utf8]);
                }
            }
        }
        for pair in page.windows(2) {
            let [previous, next] = pair else { continue; };
            if previous.is_rotated() || next.is_rotated() { continue; }
            let previous_text = previous.reading_text().trim();
            let next_text = next.reading_text().trim_start();
            let close_lines = next.y - previous.y <= (height * 1.9).max(0.045);
            let indented = next.x >= body_left + 0.027 && next.x >= previous.x + 0.025
                && ends_sentence(previous_text);
            let footer = next.y >= 0.90 && next_text.chars().all(|ch| ch.is_ascii_digit() || ch.is_whitespace() || ch == '-');
            if !close_lines || indented || footer {
                let offset = next.source_span.expect("verified OCR span").start_utf8;
                if document.text.is_char_boundary(offset) { geometry_breaks.push(offset); }
            }
        }
        geometry_breaks.sort_unstable();
        geometry_breaks.dedup();
        for (offset, heading) in transcript_ranges(&document.text, page_start..page_end, true) {
            let mut begin = offset.start;
            let first = geometry_breaks.partition_point(|&cut| cut <= begin);
            let last = geometry_breaks.partition_point(|&cut| cut < offset.end);
            for end in geometry_breaks[first..last].iter().copied().chain(std::iter::once(offset.end)) {
                ranges.push(BlockRange {
                    offset: begin..end, page: pdf.then_some(page_number), heading,
                    footnote: unnumbered_notes.contains(&begin)
                        || numbered_notes.iter().any(|note| note.contains(&begin)),
                    ..BlockRange::default()
                });
                begin = end;
            }
        }
    }

    ranges.retain(|range| document.text.get(range.offset.clone()).is_some());
    let blocks = materialize_blocks(&document.text, ranges);
    if !pdf && !blocks.is_empty() {
        document.blocks.clear();
    } else {
        document.blocks.retain(|block| block.page.is_none_or(|page| !starts.contains(&page)));
    }
    document.blocks.extend(blocks);
    document.blocks.sort_by_key(|block| block.start_utf8);
}

fn numbered_ocr_footnotes(page: &[crate::ocr::OcrLine]) -> Vec<Range<usize>> {
    let mut body_sizes: Vec<_> = page.iter()
        .filter(|line| !line.is_rotated() && line.y < 0.72 && line.width >= 0.20)
        .map(|line| line.height).collect();
    if body_sizes.len() < 3 {
        return Vec::new();
    }
    body_sizes.sort_by(f64::total_cmp);
    let body_height = body_sizes[body_sizes.len() / 2];
    let is_note_sized = |line: &crate::ocr::OcrLine| {
        !line.is_rotated() && line.y >= 0.72 && line.height <= body_height * 0.83
    };
    let Some(first) = (1..page.len()).find(|&index| {
        let (line, previous) = (&page[index], &page[index - 1]);
        is_note_sized(line) && line.y - (previous.y + previous.height) >= body_height * 0.55
            && numbered_note_label(line.reading_text()).is_some_and(|label| {
                page[..index].iter().any(|line| !pdf_note_references(line.reading_text(), &[label]).is_empty())
            })
    }) else {
        return Vec::new();
    };

    let mut notes = Vec::<Range<usize>>::new();
    for (index, line) in page.iter().enumerate().skip(first) {
        let text = line.reading_text();
        let previous = &page[index - 1];
        if !is_note_sized(line) || !text.chars().any(char::is_alphabetic) ||
            index != first && line.y - (previous.y + previous.height) > body_height * 0.95 {
            break;
        }
        let span = line.source_span.expect("verified OCR span");
        if numbered_note_label(text).is_some() {
            notes.push(span.start_utf8..span.end_utf8);
        } else if let Some(note) = notes.last_mut() {
            note.end = span.end_utf8;
        }
    }
    notes
}

fn numbered_note_label(text: &str) -> Option<u32> {
    let mut digits = text.chars().map_while(note_digit);
    let (first, superscript) = digits.next()?;
    let (number, len) = digits.try_fold((first, 1), |(number, len), (digit, kind)| {
        (len < 3 && kind == superscript).then_some((number * 10 + digit, len + 1))
    })?;
    let remainder = &text[text.char_indices().nth(len)?.0..];
    let remainder = remainder.strip_prefix(['.', ')', ']']).unwrap_or(remainder);
    (number > 0 && remainder.starts_with(char::is_whitespace)
        && remainder.trim_start().starts_with(char::is_alphabetic)).then_some(number)
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
            utf16 += if page == line.page && !line.block_start { 1 } else { 2 };
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
    let mut blocks: Vec<_> = ranges
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
                footnote_references: Vec::new(),
                footnote: range.footnote ||
                    range.page.is_some() && text[offset.clone()].lines().next().is_some_and(footnote_start),
                list: range.list,
                table_cell: range.table_cell,
            })
        })
        .collect();
    for page in blocks.chunk_by_mut(|left, right| left.page == right.page) {
        if page[0].page.is_none() { continue; }
        let numbers: Vec<_> = page.iter().filter(|block| block.footnote).filter_map(|block|
            numbered_note_label(&text[block.start_utf8..block.end_utf8])
        ).collect();
        if numbers.is_empty() { continue; }
        for block in page.iter_mut().filter(|block| !block.footnote) {
            let span = &text[block.start_utf8..block.end_utf8];
            block.footnote_references = pdf_note_references(span, &numbers)
                .into_iter().map(|range| crate::ocr::OcrSpan {
                    start_utf8: block.start_utf8 + range.start,
                    end_utf8: block.start_utf8 + range.end,
                    start_utf16: block.start_utf16 + span[..range.start].encode_utf16().count(),
                    end_utf16: block.start_utf16 + span[..range.end].encode_utf16().count(),
                }).collect();
        }
    }
    blocks
}

pub(crate) fn pdf_note_references(text: &str, numbers: &[u32]) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    let mut letters = 0;
    let mut lower = true;
    let mut after_punctuation = false;
    let mut chars = text.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        if let Some((mut number, superscript)) = note_digit(ch) {
            while let Some(digit) = chars.peek().and_then(|&(_, ch)| note_digit(ch).filter(|&(_, sup)| sup == superscript)) {
                chars.next();
                number = number.saturating_mul(10).saturating_add(digit.0);
            }
            let end = chars.peek().map_or(text.len(), |&(index, _)| index);
            if (superscript && (letters > 0 || after_punctuation) || letters >= 3 && lower) &&
                chars.peek().is_none_or(|&(_, ch)| !ch.is_alphanumeric()) && numbers.contains(&number)
            {
                result.push(start..end);
            }
        }
        if ch.is_alphabetic() {
            letters += 1;
            lower &= ch.is_lowercase();
            after_punctuation = false;
        } else {
            after_punctuation = matches!(ch, '.' | '!' | '?' | '…' | ')' | ']' | '»' | '”')
                && (letters > 0 || after_punctuation);
            letters = 0;
            lower = true;
        }
    }
    result
}

pub(crate) fn mask_pdf_non_text(document: &Document, text: &str, start: usize) -> Option<String> {
    mask_pdf_markers(document, text, start, false)
}

pub(crate) fn mask_pdf_translation_input(document: &Document, text: &str, start: usize) -> Option<String> {
    mask_pdf_markers(document, text, start, true)
}

fn mask_pdf_markers(document: &Document, text: &str, start: usize, mask_list_labels: bool) -> Option<String> {
    if document.format != "pdf" { return None; }
    let end = start.checked_add(text.len())?;
    let first = document.blocks.partition_point(|block| block.end_utf8 <= start);
    let refs = document.blocks[first..].iter().take_while(|block| block.start_utf8 < end)
        .flat_map(|block| &block.footnote_references)
        .filter(|reference| reference.start_utf8 >= start && reference.end_utf8 <= end)
        .map(|reference| reference.start_utf8 - start..reference.end_utf8 - start);
    let mut ranges: Vec<_> = refs.collect();
    if mask_list_labels {
        ranges.extend(pdf_list_label_ranges(document, text, start));
    }
    ranges.sort_unstable_by_key(|range| range.start);
    let mut clean = String::new();
    let mut copied = 0;
    for mut range in ranges {
        range.start = range.start.max(copied);
        if range.is_empty() { continue; }
        let marker = text.get(range.clone())?;
        clean.push_str(text.get(copied..range.start)?);
        clean.extend(std::iter::repeat_n(' ', marker.encode_utf16().count()));
        copied = range.end;
    }
    if copied == 0 && !text.contains('\u{fffc}') { return None; }
    clean.push_str(text.get(copied..)?);
    // Keep UTF-16 offsets used for OCR geometry while omitting PDFKit artwork placeholders.
    Some(clean.replace('\u{fffc}', " "))
}

pub(crate) fn skip_in_translation(document: &Document, sentence: &Sentence) -> bool {
    sentence_in_footnote(&document.blocks, sentence)
        || (document.format == "pdf" &&
            sentence.text.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{fffc}').is_empty())
        || (document.format == "pdf" && sentence.text.len() <= 32 &&
            mask_pdf_translation_input(document, &sentence.text, sentence.start_utf8)
                .is_some_and(|text| text.trim().is_empty()))
}

pub(crate) fn pdf_list_label_ranges<'a>(document: &'a Document, text: &'a str, start: usize)
    -> impl Iterator<Item = Range<usize>> + 'a
{
    let mut line_offset = 0;
    text.split_inclusive('\n').filter_map(move |line| {
        let prefix = line_offset;
        line_offset += line.len();
        let body = line.trim_start_matches(|ch: char| ch.is_whitespace() || ch == '\u{fffc}');
        let begin = prefix + line.len() - body.len();
        let token = body.split_whitespace().next()?;
        let label = token.strip_suffix('.')?.as_bytes();
        if !(label.len() == 1 && label[0].is_ascii_lowercase())
            && !((1..=3).contains(&label.len()) && label.iter().all(u8::is_ascii_digit))
        {
            return None;
        }
        let absolute = start.checked_add(begin)?;
        let original = document.text.get(absolute..)?.strip_prefix(token)?;
        if !original.starts_with(|ch: char| ch.is_whitespace() && !matches!(ch, '\r' | '\n')) {
            return None;
        }
        let before = document.text.get(..absolute)?.rsplit(['\r', '\n']).next()?;
        before.chars().all(|ch| ch.is_whitespace() || ch == '\u{fffc}')
            .then_some(begin..begin + token.len())
    })
}

pub(crate) fn note_digit(ch: char) -> Option<(u32, bool)> {
    if let Some(digit) = ch.to_digit(10) {
        return Some((digit, false));
    }
    "⁰¹²³⁴⁵⁶⁷⁸⁹".chars().position(|digit| digit == ch).map(|index| (index as u32, true))
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
            transcript_ranges(&text, span, true).into_iter().map(|(offset, heading)| BlockRange {
                offset,
                page: Some(page),
                heading,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::{OcrLine, OcrSpan};

    #[test]
    fn unicode_txt_uses_one_transcript_for_language_detection_and_backup() {
        use crate::{language_detection, Store};

        let text = "Бібліотека 📖 відчинена. Сьогодні я читаю книжку українською мовою.";
        let mut little16 = vec![0xff, 0xfe];
        let mut big16 = vec![0xfe, 0xff];
        for unit in text.encode_utf16() {
            little16.extend(unit.to_le_bytes());
            big16.extend(unit.to_be_bytes());
        }
        let mut little32 = vec![0xff, 0xfe, 0, 0];
        let mut big32 = vec![0, 0, 0xfe, 0xff];
        for character in text.chars().map(u32::from) {
            little32.extend(character.to_le_bytes());
            big32.extend(character.to_be_bytes());
        }

        let mut store = Store::open(Path::new(":memory:")).unwrap();
        for bytes in [&little16, &big16, &little32, &big32] {
            assert_eq!(language_detection::detect_document("book.txt", bytes), Some("uk"));
            let staged = store.stage_intake(crate::intake::IntakeInput::File {
                name: "book.txt".into(), bytes: bytes.to_vec(),
            }).unwrap();
            store.complete_intake(&staged.id, crate::intake::IntakeImport::Document {
                language: Some("uk".into()),
            }).unwrap();
        }
        let mut restored = Store::open(Path::new(":memory:")).unwrap();
        restored.restore(store.export().unwrap()).unwrap();
        let documents = restored.snapshot().unwrap().documents;
        assert_eq!(documents.len(), 4);
        for document in documents {
            assert_eq!(document.text, text);
            let original = restored.original(&document.id).unwrap();
            assert!([&little16, &big16, &little32, &big32].contains(&&original.bytes));
        }
        assert!(parse("broken.txt", &[0xff, 0xfe, 0x41]).is_err());
        assert!(parse("broken.txt", &[0xfe, 0xff, 0xd8, 0x00]).is_err());
        assert!(parse("broken.txt", &[0xff, 0xfe, 0, 0, 0x41]).is_err());
        assert!(parse("broken.txt", &[0, 0, 0xfe, 0xff, 0, 0x11, 0, 0]).is_err());
    }

    #[test]
    fn flattened_ocr_keeps_unpunctuated_dialogue_turns_and_inline_asides() {
        let raw = "— As-tu vu le bateau — Oui — dit-il — je rentre — Alors viens !\n« Je reste ici » Le marin descend.\n\nIl part — Paris l’attend.";
        let document = detached_pdf_document(raw.into(), "fr", None);
        let excerpts: Vec<_> = document.blocks.iter().map(|block| {
            assert_eq!(block.start_utf16, raw[..block.start_utf8].encode_utf16().count());
            assert_eq!(block.end_utf16, raw[..block.end_utf8].encode_utf16().count());
            &raw[block.start_utf8..block.end_utf8]
        }).collect();
        assert_eq!(excerpts, [
            "— As-tu vu le bateau",
            "— Oui — dit-il — je rentre",
            "— Alors viens !",
            "« Je reste ici »",
            "Le marin descend.",
            "Il part — Paris l’attend.",
        ]);
        assert_eq!(document.text, raw);
    }

    #[test]
    fn small_referenced_scan_footnotes_include_wrapped_lines_and_keep_offsets() {
        let rows = [
            ("📖 Le navire aborde à Naples.", 0.18, 0.030),
            ("L’équipage⁷ attend sur le quai.", 0.23, 0.030),
            ("Le capitaine8 reste à bord.", 0.28, 0.030),
            ("7. Les marins viennent de Marseille.", 0.76, 0.018),
            ("Ils connaissent la mer depuis longtemps.", 0.782, 0.018),
            ("8) Le capitaine commande le navire.", 0.804, 0.018),
            ("42", 0.94, 0.016),
        ];
        let raw = rows.iter().map(|(text, _, _)| *text).collect::<Vec<_>>().join("\n");
        let mut offset = 0;
        let lines: Vec<_> = rows.into_iter().map(|(text, y, height)| {
            let start = offset;
            let end = start + text.len();
            offset = end + 1;
            OcrLine {
                rotation: None,
                block_start: false,
                page: 5, text: text.into(), x: 0.12, y, width: 0.72, height,
                confidence: None, correction: None, words: Vec::new(),
                source_span: Some(OcrSpan {
                    start_utf8: start, end_utf8: end,
                    start_utf16: raw[..start].encode_utf16().count(),
                    end_utf16: raw[..end].encode_utf16().count(),
                }),
            }
        }).collect();
        let document = detached_pdf_document(raw.clone(), "fr", Some(lines.clone()));
        let notes: Vec<_> = document.blocks.iter().filter(|block| block.footnote).map(|block| {
            assert_eq!(block.page, Some(5));
            assert_eq!(block.start_utf16, raw[..block.start_utf8].encode_utf16().count());
            &document.text[block.start_utf8..block.end_utf8]
        }).collect();
        assert_eq!(notes, ["7. Les marins viennent de Marseille.\nIls connaissent la mer depuis longtemps.",
            "8) Le capitaine commande le navire."]);
        assert_eq!(document.text, raw);
        let masked = mask_pdf_non_text(&document, &raw, 0).unwrap();
        assert!(masked.contains("L’équipage  attend") && masked.contains("capitaine  reste"));
        assert_eq!(masked.encode_utf16().count(), raw.encode_utf16().count());

        let mut ordinary = lines;
        for line in &mut ordinary[3..6] { line.height = 0.030; }
        let document = detached_pdf_document(raw.clone(), "fr", Some(ordinary));
        assert!(document.blocks.iter().all(|block| !block.footnote));
        assert!(mask_pdf_non_text(&document, &raw, 0).is_none());
    }

    #[test]
    fn native_pdf_footnote_references_survive_proper_names_and_sentence_punctuation() {
        let input = "Napoleon⁷ arrive au port.\nIl parle au capitaine.⁸\nLes avions A320 et V2 attendent.\n7 Napoleon : empereur.\n8 Capitaine : personne qui dirige.";
        let document = detached_pdf_document(input.into(), "fr", None);
        let references = document.blocks.iter().flat_map(|block| &block.footnote_references)
            .map(|span| &input[span.start_utf8..span.end_utf8]).collect::<Vec<_>>();
        assert_eq!(references, ["⁷", "⁸"]);
        let masked = mask_pdf_non_text(&document, input, 0).unwrap();
        assert!(masked.contains("Napoleon  arrive") && masked.contains("capitaine. "));
        assert!(masked.contains("A320 et V2"));
        assert_eq!(masked.encode_utf16().count(), input.encode_utf16().count());
    }

    #[test]
    fn pdf_artwork_placeholders_keep_offsets_without_entering_translation() {
        let raw = "\u{fffc}\n11. Préparez votre essai.\n\u{fffc}méthodologique";
        let document = detached_pdf_document(raw.into(), "fr", None);
        let cleaned = mask_pdf_translation_input(&document, raw, 0).unwrap();
        assert_eq!(cleaned, " \n    Préparez votre essai.\n méthodologique");
        assert_eq!(cleaned.encode_utf16().count(), raw.encode_utf16().count());
        assert_eq!(document.text, raw);
    }

    #[test]
    fn pdf_layout_keeps_inline_dialogue_and_footnotes_when_ocr_boxes_are_present() {
        let raw = "CHAPITRE 1\n📖 Le marin descend. – Où êtes-vous ? – Sur le quai. 1 Quai : lieu d’accostage.";
        let lines = raw.split_inclusive('\n').scan(0, |offset, line| {
            let first = *offset;
            *offset += line.len();
            let last = first + line.trim_end().len();
            Some(OcrLine {
                rotation: None,
                block_start: false,
                page: 0,
                text: raw[first..last].into(),
                x: 0.1,
                y: if first == 0 { 0.1 } else { 0.14 },
                width: 0.8,
                height: 0.04,
                confidence: None,
                correction: None,
                words: Vec::new(),
                source_span: Some(OcrSpan {
                    start_utf8: first,
                    end_utf8: last,
                    start_utf16: raw[..first].encode_utf16().count(),
                    end_utf16: raw[..last].encode_utf16().count(),
                }),
            })
        }).collect();
        let doc = detached_pdf_document(raw.into(), "fr", Some(lines));
        let blocks: Vec<_> = doc.blocks.iter().map(|block|
            (&doc.text[block.start_utf8..block.end_utf8], block.heading_level, block.footnote)
        ).collect();
        assert_eq!(blocks, [
            ("CHAPITRE 1", Some(1), false),
            ("📖 Le marin descend.", None, false),
            ("– Où êtes-vous ?", None, false),
            ("– Sur le quai.", None, false),
            ("1 Quai : lieu d’accostage.", None, true),
        ]);
        assert_eq!(doc.text, raw);
        for block in doc.blocks {
            assert_eq!(block.start_utf16, raw[..block.start_utf8].encode_utf16().count());
            assert_eq!(block.end_utf16, raw[..block.end_utf8].encode_utf16().count());
        }
    }

    #[test]
    fn flattened_pdf_separates_quotes_and_ascii_dash_turns_without_changing_source() {
        let raw = "📖 Elle demande : « Vous venez ? » Le marin hésite. - Oui, M. Morrel. - Au port, alors !\nLe tarif de l’aller-retour est 12 - 3 euros.";
        let mut document = detached_pdf_document(raw.into(), "fr", None);
        assert_eq!(document.text, raw);
        reflow_ocr_paragraphs(&mut document);
        let blocks: Vec<_> = document.blocks.iter().map(|block| {
            assert_eq!(block.start_utf16, raw[..block.start_utf8].encode_utf16().count());
            assert_eq!(block.end_utf16, raw[..block.end_utf8].encode_utf16().count());
            &raw[block.start_utf8..block.end_utf8]
        }).collect();
        assert_eq!(blocks, [
            "📖 Elle demande :",
            "« Vous venez ? »",
            "Le marin hésite.",
            "- Oui, M. Morrel.",
            "- Au port, alors !",
            "Le tarif de l’aller-retour est 12 - 3 euros.",
        ]);
    }
}
