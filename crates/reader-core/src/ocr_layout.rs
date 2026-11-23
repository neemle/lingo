use crate::ocr::{byte_at_utf16, OcrLine, TranscriptEdit};
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

pub(crate) fn upgrade_cached_page(lines: &[OcrLine], text: &str) -> Option<(Vec<OcrLine>, String, Vec<TranscriptEdit>)> {
    let pages = lines.first()?.page.checked_add(1)?;
    if crate::ocr::recognized_lines(lines, pages).ok()? != text { return None; }
    let lines = reconstruct_page(lines)?;
    let revised = crate::ocr::recognized_lines(&lines, pages).ok()?;
    if revised == text { return None; }
    let edits = layout_edits(text, &revised);
    Some((lines, revised, edits))
}

fn layout_edits(old: &str, new: &str) -> Vec<TranscriptEdit> {
    if old.split_whitespace().eq(new.split_whitespace()) {
        let mut edits = Vec::new();
        let mut old_tail = old;
        let mut new_tail = new;
        let mut offset = 0;
        for word in old.split_whitespace() {
            let a = old_tail.find(word).expect("unchanged word");
            let b = new_tail.find(word).expect("unchanged word");
            let old_len = old_tail[..a].encode_utf16().count();
            let new_len = new_tail[..b].encode_utf16().count();
            if old_tail[..a] != new_tail[..b] {
                edits.push(TranscriptEdit { start_utf16: offset, old_len, new_len });
            }
            offset += old_len + word.encode_utf16().count();
            old_tail = &old_tail[a + word.len()..];
            new_tail = &new_tail[b + word.len()..];
        }
        return edits;
    }
    let prefix: usize = old.chars().zip(new.chars()).take_while(|(a, b)| a == b)
        .map(|(ch, _)| ch.len_utf8()).sum();
    let suffix: usize = old[prefix..].chars().rev().zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b).map(|(ch, _)| ch.len_utf16()).sum();
    vec![TranscriptEdit {
        start_utf16: old[..prefix].encode_utf16().count(),
        old_len: old[prefix..].encode_utf16().count() - suffix,
        new_len: new[prefix..].encode_utf16().count() - suffix,
    }]
}

pub(crate) fn intersects_edits(start: usize, end: usize, edits: &[TranscriptEdit]) -> bool {
    edits.iter().any(|edit| edit.start_utf16 < end && edit.start_utf16 + edit.old_len > start)
}

fn reconstruct_page(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.correction.is_some()) {
        return None;
    }
    let mut ordered = reconstruct_columns(lines);
    let starts = ordered.as_deref().unwrap_or(lines).windows(2).enumerate()
        .filter_map(|(index, pair)| (vertical_block_break(&pair[0], &pair[1])).then_some(index + 1))
        .collect::<Vec<_>>();
    if !starts.is_empty() {
        let ordered = ordered.get_or_insert_with(|| lines.to_vec());
        for index in starts {
            ordered[index].block_start = true;
        }
    }
    ordered
}

fn vertical_block_break(upper: &OcrLine, lower: &OcrLine) -> bool {
    if lower.block_start || ![upper.height, lower.height].into_iter().all(|height| height > 0.0 && height <= 1.0) {
        return false;
    }
    let gap = lower.y - (upper.y + upper.height);
    let larger = upper.height.max(lower.height);
    let smaller = upper.height.min(lower.height);
    if gap >= 1.4 * larger {
        return true;
    }
    // A short display line can sit closer to the body than two regular blocks.
    let overlap = (upper.x + upper.width).min(lower.x + lower.width) - upper.x.max(lower.x);
    let display_line = if upper.height >= lower.height { upper } else { lower };
    gap >= 0.45 * smaller && larger >= 1.5 * smaller && overlap > 0.0 &&
        display_line.reading_text().split_whitespace().take(13).count() <= 12
}

fn reconstruct_columns(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.block_start) ||
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

    fn caption_line() -> OcrLine {
        OcrLine {
            page: 0, text: "Left caption Right caption".into(), x: 0.12, y: 0.3, width: 0.68, height: 0.02,
            confidence: None, correction: None, block_start: false, source_span: None,
            words: [("Left", 0, 0.12), ("caption", 5, 0.17), ("Right", 13, 0.60), ("caption", 19, 0.66)]
                .into_iter().map(|(text, offset, x)| OcrWord {
                    text: text.into(), x, y: 0.3, width: 0.04, height: 0.02,
                    raw_start_utf16: Some(offset), source_span: None,
                }).collect(),
        }
    }

    #[test]
    fn scanned_headings_and_captions_bound_soft_wrapped_paragraphs() {
        let root = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&root.path().join("reader.db")).unwrap();
        let line = |text: &str, y: f64, height: f64| OcrLine {
            page: 0, text: text.into(), x: 0.15, y, width: 0.68, height,
            confidence: None, correction: None, block_start: false, source_span: None, words: Vec::new(),
        };
        let lines = vec![
            line("Un capitaine de vingt ans", 0.1, 0.045),
            line("Le bateau entre dans", 0.158, 0.024),
            line("le port de Marseille.", 0.187, 0.024),
            line("Une illustration du port", 0.55, 0.021),
        ];
        let doc = store.import_scanned_pdf("scan.pdf", b"%PDF-1.4\n", "fr", lines.clone(), 1).unwrap();
        assert_eq!(doc.text, "Un capitaine de vingt ans\n\nLe bateau entre dans\nle port de Marseille.\n\nUne illustration du port");
        assert_eq!(doc.sentences.len(), 3);
        let saved = doc.ocr_lines.unwrap();
        assert_eq!(saved.iter().map(|line| line.block_start).collect::<Vec<_>>(), [false, true, false, true]);
        let source = saved[3].source_span.unwrap();
        assert_eq!(&doc.text[source.start_utf8..source.end_utf8], "Une illustration du port");

        // A prior column split is already stored. The newer vertical pass can still upgrade it.
        let mut cached = lines;
        cached[3].block_start = true;
        let old = crate::ocr::recognized_lines(&cached, 1).unwrap();
        let (_, revised, edits) = upgrade_cached_page(&cached, &old).unwrap();
        assert_eq!(revised, doc.text);
        assert_eq!(edits.len(), 1);
    }

    #[test]
    fn cached_scan_layout_preserves_protected_anchors_before_upgrading() {
        let root = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&root.path().join("reader.db")).unwrap();
        let original = root.path().join("scan.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut line = caption_line();
        line.block_start = true;
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en", vec![String::new()], vec![line],
        ).unwrap();
        let doc = store.import_pdf_file(prepared).unwrap();
        assert_eq!(doc.text, "Left caption Right caption");
        // Simulate an older import without a layout marker, retaining the saved OCR geometry.
        store.connection.execute(
            "UPDATE documents SET body=json_set(body, '$.ocrLines[0].blockStart', json('false')) WHERE id=?1",
            [&doc.id],
        ).unwrap();
        let revision = doc.current_revision_id.as_deref().unwrap();
        let sentence = &doc.sentences[0].id;
        store.set_bookmark(&doc.id, sentence, true).unwrap();
        assert!(!store.upgrade_stored_ocr_layout(&doc.id, 0, revision, &doc.text, false).unwrap());
        store.set_bookmark(&doc.id, sentence, false).unwrap();
        assert!(store.upgrade_stored_ocr_layout(&doc.id, 0, revision, &doc.text, false).unwrap());
        let window = store.pdf_read_window(&doc.id, 0).unwrap();
        assert_eq!(window.pages[0].text, "Left caption\n\nRight caption");
        assert!(!store.upgrade_stored_ocr_layout(&doc.id, 0, revision, &doc.text, false).unwrap());
    }

    #[test]
    fn imported_scan_keeps_independent_captions_and_word_positions() {
        let root = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&root.path().join("reader.db")).unwrap();
        let line = caption_line();
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
