use crate::ocr::{byte_at_utf16, OcrLine};
use crate::pdf_text_blocks::{separate_blocks, PositionedWord};
use std::ops::Range;

/// Applies shared page-layout decisions before recognition results acquire canonical anchors.
pub(crate) fn reconstruct_blocks(lines: &mut Vec<OcrLine>) {
    let mut result = Vec::with_capacity(lines.len());
    let mut input = std::mem::take(lines).into_iter().peekable();
    while let Some(first) = input.next() {
        let page_number = first.page;
        let mut page = vec![first];
        while let Some(line) = input.next_if(|line| line.page == page_number) {
            page.push(line);
        }
        result.extend(reconstruct_page(&page).unwrap_or(page));
    }
    *lines = result;
}

fn reconstruct_page(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.block_start || line.correction.is_some()) ||
        !lines.iter().any(|line| line.words.len() >= 4) && lines.len() < 2 {
        return None;
    }
    let mut text = String::new();
    let mut spans = Vec::with_capacity(lines.len());
    let mut positioned = Vec::new();
    let mut utf16 = 0;
    for line in lines {
        if !text.is_empty() {
            text.push('\n');
            utf16 += 1;
        }
        let raw = line.reading_text();
        let units = raw.encode_utf16().collect::<Vec<_>>();
        let first_word = positioned.len();
        for word in &line.words {
            let Some(start) = word.raw_start_utf16 else { continue; };
            let Some(end) = start.checked_add(word.text.encode_utf16().count()) else { continue; };
            if !units.get(start..end).is_some_and(|slice| word.text.encode_utf16().eq(slice.iter().copied())) {
                continue;
            }
            let whitespace = |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace);
            let left = units[..start].iter().rposition(whitespace).map_or(0, |index| index + 1);
            let right = units[end..].iter().position(whitespace).map_or(units.len(), |index| end + index);
            let geometry = PositionedWord {
                start: utf16 + left, end: utf16 + right,
                left: word.x, right: word.x + word.width,
                center_y: word.y + word.height / 2.0, height: word.height,
            };
            if positioned.len() > first_word &&
                positioned.last().is_some_and(|last: &PositionedWord| last.start == geometry.start) {
                let prior = positioned.last_mut()?;
                prior.left = prior.left.min(geometry.left);
                prior.right = prior.right.max(geometry.right);
            } else {
                positioned.push(geometry);
            }
        }
        spans.push(utf16..utf16 + units.len());
        utf16 += units.len();
        if utf16 > 24_000 { return None; }
        text.push_str(raw);
    }
    positioned.sort_unstable_by_key(|word| word.start);
    let edits = separate_blocks(&text, &positioned, 1.0).ok()?;
    if edits.is_empty() { return None; }

    let mut output = Vec::with_capacity(lines.len());
    let mut cursor = 0;
    let mut new_block = false;
    for edit in edits {
        append(lines, &spans, cursor..edit.start, &mut new_block, &mut output)?;
        if edit.slices.is_empty() {
            new_block = true;
        } else {
            for part in edit.slices {
                new_block |= part.separator == "\n\n";
                append(lines, &spans, part.start..part.end, &mut new_block, &mut output)?;
            }
            new_block = true;
        }
        cursor = edit.end;
    }
    append(lines, &spans, cursor..utf16, &mut new_block, &mut output)?;
    Some(output)
}

fn append(
    lines: &[OcrLine], spans: &[Range<usize>], range: Range<usize>, new_block: &mut bool, output: &mut Vec<OcrLine>,
) -> Option<()> {
    let first = spans.partition_point(|span| span.end <= range.start);
    for (line, span) in lines[first..].iter().zip(&spans[first..]).take_while(|(_, span)| span.start < range.end) {
        let start = range.start.max(span.start) - span.start;
        let end = range.end.min(span.end) - span.start;
        if start >= end { continue; }
        let mut piece = if start == 0 && end == span.len() {
            line.clone()
        } else {
            slice_line(line, start, end)?
        };
        if *new_block && !output.is_empty() {
            piece.block_start = true;
        }
        *new_block = false;
        output.push(piece);
    }
    Some(())
}

fn slice_line(line: &OcrLine, start: usize, end: usize) -> Option<OcrLine> {
    let raw = line.reading_text();
    let a = byte_at_utf16(raw, start)?;
    let b = byte_at_utf16(raw, end)?;
    let slice = raw.get(a..b)?;
    let trimmed = slice.trim();
    let leading = slice[..slice.len() - slice.trim_start().len()].encode_utf16().count();
    let offset = start + leading;
    let mut piece = line.clone();
    piece.text = trimmed.to_owned();
    piece.words.clear();
    piece.source_span = None;
    for word in &line.words {
        let position = word.raw_start_utf16?;
        let word_end = position.checked_add(word.text.encode_utf16().count())?;
        if position >= end || word_end <= start { continue; }
        if position < offset || word_end > offset + trimmed.encode_utf16().count() { return None; }
        let mut word = word.clone();
        word.raw_start_utf16 = Some(position - offset);
        word.source_span = None;
        piece.words.push(word);
    }
    let first = piece.words.first()?;
    let (mut left, mut top, mut right, mut bottom) = (first.x, first.y, first.x + first.width, first.y + first.height);
    for word in &piece.words[1..] {
        left = left.min(word.x);
        top = top.min(word.y);
        right = right.max(word.x + word.width);
        bottom = bottom.max(word.y + word.height);
    }
    piece.x = left;
    piece.y = top;
    piece.width = right - left;
    piece.height = bottom - top;
    if piece.width > 1.0 || piece.height > 1.0 { return None; }
    Some(piece)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    #[test]
    fn imported_scan_keeps_independent_captions_and_word_positions() {
        let root = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&root.path().join("reader.db")).unwrap();
        let line = OcrLine {
            page: 0, text: "Left caption Right caption".into(), x: 0.12, y: 0.3, width: 0.68, height: 0.02,
            confidence: None, correction: None, block_start: false, source_span: None,
            words: [("Left", 0, 0.12), ("caption", 5, 0.17), ("Right", 13, 0.60), ("caption", 19, 0.66)]
                .into_iter().map(|(text, offset, x)| OcrWord {
                    text: text.into(), x, y: 0.3, width: 0.04, height: 0.02,
                    raw_start_utf16: Some(offset), source_span: None,
                }).collect(),
        };
        let document = store.import_scanned_pdf("scan.pdf", b"%PDF-1.4\n", "en", vec![line.clone()], 1).unwrap();
        assert_eq!(document.text, "Left caption\n\nRight caption");
        assert_eq!(document.sentences.len(), 2);
        let lines = document.ocr_lines.unwrap();
        assert!(lines[1].block_start);
        assert_eq!(lines[1].words[0].x, 0.60);
        assert_eq!(lines[1].words[0].source_span.unwrap().start_utf16, 14);

        let mut left = slice_line(&line, 0, 12).unwrap();
        let mut right = slice_line(&line, 13, 26).unwrap();
        left.text.push('.');
        right.text.push('!');
        let photo = store.import_image("scan.png", b"\x89PNG\r\n\x1a\n", "en", vec![left, right]).unwrap();
        assert_eq!(photo.text, "Left caption.\n\nRight caption!");
        let lines = photo.ocr_lines.unwrap();
        assert!(lines[1].block_start);
        assert_eq!(lines[1].words[0].source_span.unwrap().start_utf16, 15);
    }
}
