use crate::ocr::{byte_at_utf16, OcrLine, TranscriptEdit};
use crate::pdf_text_blocks::{separate_blocks, PositionedWord};
use std::ops::Range;

mod drop_caps;
pub(crate) use drop_caps::recover_sparse_initials;

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
        result.extend(reconstruct_with_drop_caps(&page).unwrap_or(page));
    }
    *lines = result;
}

pub(crate) fn upgrade_cached_page(lines: &[OcrLine], text: &str) -> Option<(Vec<OcrLine>, String, Vec<TranscriptEdit>)> {
    let pages = lines.first()?.page.checked_add(1)?;
    if crate::ocr::recognized_lines(lines, pages).ok()? != text { return None; }
    let lines = reconstruct_with_drop_caps(lines)?;
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

/// Fill holes in saved OCR without replacing existing lines or their corrections.
pub(crate) fn supplement_page(
    saved: &[OcrLine], incoming: Vec<OcrLine>,
) -> crate::Result<Option<(Vec<OcrLine>, Vec<TranscriptEdit>)>> {
    let mut merged = crate::ocr::merge_missing_lines(saved.to_vec(), incoming);
    if merged.len() == saved.len() {
        return Ok(None);
    }
    // Corrections inhibit the generic reorder pass. Their blocks and word
    // coordinates stay attached to the original lines.
    if let Some(reconstructed) = reconstruct_page(&merged) {
        merged = reconstructed;
    }
    let mut edits = Vec::new();
    let mut old_end = 0;
    let mut rows = merged.iter().enumerate();
    for (index, original) in saved.iter().enumerate() {
        let old_gap = separator_width(index, original);
        let mut new_gap = 0;
        loop {
            let (new_index, row) = rows.next()
                .ok_or("Нове OCR змінює порядок збережених рядків.")?;
            new_gap += separator_width(new_index, row);
            if row.text == original.text && row.correction == original.correction
                && row.x == original.x && row.y == original.y
            {
                break;
            }
            new_gap += row.reading_text().encode_utf16().count();
        }
        if old_gap != new_gap {
            edits.push(TranscriptEdit { start_utf16: old_end, old_len: old_gap, new_len: new_gap });
        }
        let length = original.reading_text().encode_utf16().count();
        old_end += old_gap + length;
    }
    let length: usize = rows.map(|(index, row)|
        separator_width(index, row) + row.reading_text().encode_utf16().count()).sum();
    if length > 0 {
        edits.push(TranscriptEdit { start_utf16: old_end, old_len: 0, new_len: length });
    }
    Ok(Some((merged, edits)))
}

fn separator_width(index: usize, line: &OcrLine) -> usize {
    if index == 0 { 0 } else { 1 + usize::from(line.block_start) }
}

fn reconstruct_with_drop_caps(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.correction.is_some()) { return None; }
    match drop_caps::join(lines) {
        Some(joined) => Some(reconstruct_page(&joined).unwrap_or(joined)),
        None => reconstruct_page(lines),
    }
}

fn reconstruct_page(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.correction.is_some()) {
        return None;
    }
    let mut ordered = None;
    let mut start = 0;
    for block in lines.chunk_by(|previous, next| !next.block_start && previous.is_rotated() == next.is_rotated()) {
        let rebuilt = if block[0].is_rotated() { None } else { reconstruct_columns(block) };
        if let Some(rebuilt) = rebuilt {
            ordered.get_or_insert_with(|| lines[..start].to_vec()).extend(rebuilt);
        } else if let Some(ordered) = &mut ordered {
            ordered.extend_from_slice(block);
        }
        start += block.len();
    }
    let starts = ordered.as_deref().unwrap_or(lines).windows(2).enumerate()
        .filter_map(|(index, pair)| {
            (pair[0].is_rotated() != pair[1].is_rotated()
                || (!pair[0].is_rotated() && vertical_block_break(&pair[0], &pair[1])))
                .then_some(index + 1)
        })
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
    if lines.len() < 2 && !lines.iter().any(|line| line.words.len() >= 4) {
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
    piece.block_start &= start == 0;
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
    fn scanned_drop_cap_joins_word_and_updates_geometry() {
        let mut cap = caption_line();
        cap.text = "L".into();
        cap.x = 0.08;
        cap.y = 0.295;
        cap.width = 0.035;
        cap.height = 0.046;
        cap.words.clear();
        let mut body = caption_line();
        body.text = "e bateau".into();
        body.words.truncate(2);
        body.words[0].text = "e".into();
        body.words[1].text = "bateau".into();
        body.words[1].raw_start_utf16 = Some(2);
        let mut lines = vec![body, cap];
        reconstruct_blocks(&mut lines);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Le bateau");
        assert_eq!(lines[0].words[0].text, "Le");
        assert_eq!(lines[0].words[0].x, 0.08);
        assert_eq!(lines[0].words[1].raw_start_utf16, Some(3));
    }

    #[test]
    fn inline_drop_cap_joins_located_words_without_merging_regular_initials() {
        let mut line = caption_line();
        line.text = "L e bateau".into();
        line.words.truncate(3);
        for (word, (text, start, x)) in line.words.iter_mut().zip([
            ("L", 0, 0.12), ("e", 2, 0.16), ("bateau", 4, 0.22),
        ]) {
            word.text = text.into();
            word.raw_start_utf16 = Some(start);
            word.x = x;
        }
        line.words[0].width = 0.035;
        let mut regular = vec![line.clone()];
        reconstruct_blocks(&mut regular);
        assert_eq!(regular[0].text, "L e bateau");

        line.words[0].height = 0.045;
        line.words[0].y = 0.295;
        let mut lines = vec![line];
        reconstruct_blocks(&mut lines);
        assert_eq!(lines[0].text, "Le bateau");
        crate::ocr::refresh_line_provenance("Le bateau", &mut lines, None);
        let first = &lines[0].words[0];
        assert_eq!(first.text, "Le");
        assert_eq!(first.x, 0.12);
        assert!(first.x + first.width >= 0.20);
        assert_eq!(first.source_span.unwrap().end_utf16, 2);
        assert_eq!(lines[0].words[1].source_span.unwrap().start_utf16, 3);
    }

    fn caption_line() -> OcrLine {
        OcrLine {
            rotation: None,
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
            rotation: None,
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
        let line = caption_line();
        let legacy_words = serde_json::to_string(&line.words).unwrap();
        let mut line_without_geometry = line;
        line_without_geometry.words.clear();
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en", vec![String::new()], vec![line_without_geometry],
        ).unwrap();
        let doc = store.import_pdf_file(prepared).unwrap();
        assert_eq!(doc.text, "Left caption Right caption");
        // Simulate an older import that stored the geometry without separating the captions.
        store.connection.execute(
            "UPDATE documents SET body=json_set(body, '$.ocrLines[0].words', json(?2)) WHERE id=?1",
            rusqlite::params![doc.id, legacy_words],
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
    fn marked_ocr_block_still_separates_side_by_side_captions() {
        let mut heading = caption_line();
        heading.text = "Choose an option".into();
        heading.words.clear();
        heading.y = 0.26;
        let mut captions = caption_line();
        captions.block_start = true;
        let mut page = vec![heading, captions];

        reconstruct_blocks(&mut page);

        let text = "Choose an option\n\nLeft caption\n\nRight caption";
        assert_eq!(crate::ocr::recognized_lines(&page, 1).unwrap(), text);
        assert_eq!(page[2].words[0].raw_start_utf16, Some(0));
        assert!(upgrade_cached_page(&page, text).is_none());
    }

    #[test]
    fn cached_ocr_supplement_keeps_a_bookmark_after_an_inserted_row() {
        let root = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&root.path().join("reader.db")).unwrap();
        let original = root.path().join("scan.pdf");
        std::fs::write(&original, b"%PDF-1.4\n").unwrap();
        let mut first = caption_line();
        first.text = "The first paragraph ends.".into();
        first.words.clear();
        first.y = 0.15;
        let mut last = first.clone();
        last.text = "The last paragraph ends.".into();
        last.y = 0.55;
        let prepared = crate::file_originals::prepare_pdf_import(
            store.pdf_original_root(), "scan.pdf", &original, "en",
            vec![String::new()], vec![first, last],
        ).unwrap();
        let document = store.import_pdf_file(prepared).unwrap();
        store.set_bookmark(&document.id, &document.sentences[1].id, true).unwrap();
        let mut recovered = caption_line();
        recovered.text = "Left caption.".into();
        recovered.words.truncate(2);
        recovered.words[1].text.push('.');
        recovered.width = 0.09;
        recovered.confidence = Some(0.98);
        let revision = document.current_revision_id.as_deref().unwrap();
        assert!(store.supplement_pdf_ocr_page(&document.id, 0, revision, "en", false, vec![recovered]).unwrap());
        let refreshed = store.pdf_read_window(&document.id, 0).unwrap();
        assert!(refreshed.pages[0].text.contains("Left caption."));
        let saved = store.translation_document(&document.id).unwrap();
        let last = saved.sentences.iter().find(|sentence| sentence.text == "The last paragraph ends.").unwrap();
        assert_eq!(saved.bookmarks, [last.id.clone()]);
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
