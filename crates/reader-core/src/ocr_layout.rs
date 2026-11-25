use crate::ocr::{byte_at_utf16, OcrLine, TranscriptEdit};
use crate::pdf_text_blocks::{separate_blocks, separate_confirmed_grids, PositionedWord};
use std::ops::Range;

mod drop_caps;
mod numeric_labels;
pub(crate) use drop_caps::{corrected_initial_lengths, is_initial, is_initial_artifact, recover_sparse_initials};

pub(crate) fn repair_numeric_labels(page: &mut [OcrLine]) {
    numeric_labels::repair(page);
}

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
        group_rotated_lines(&mut page);
        let mut page = reconstruct_with_drop_caps(&page).unwrap_or(page);
        repair_numeric_labels(&mut page);
        result.extend(page);
    }
    *lines = result;
}

fn group_rotated_lines(page: &mut [OcrLine]) {
    let reading_box = OcrLine::reading_bounds;
    for paragraph in page.chunk_by_mut(|first, second| {
        if !first.is_rotated() || first.rotation != second.rotation
            || [first, second].into_iter().any(|line| {
                line.correction.is_some() || line.reading_text().split_whitespace().take(2).count() < 2
            })
        {
            return false;
        }
        let (a, b) = if reading_box(first).1 <= reading_box(second).1 { (first, second) } else { (second, first) };
        let (ax, ay, aw, ah) = reading_box(a);
        let (bx, by, bw, bh) = reading_box(b);
        let thickness = ah.min(bh);
        let overlap = (ax + aw).min(bx + bw) - ax.max(bx);
        let gap = by - (ay + ah);
        thickness > 0.0 && ah.max(bh) <= 1.45 * thickness
            && (-0.18 * thickness..=0.85 * thickness).contains(&gap)
            && (ax - bx).abs() <= 0.15 * aw.min(bw)
            && overlap >= 0.65 * aw.min(bw)
            && !crate::document_layout::keep_pdf_sentence_break(a.reading_text(), b.reading_text())
    }) {
        if !paragraph[0].is_rotated() { continue; }
        paragraph.sort_by(|first, second| reading_box(first).1.total_cmp(&reading_box(second).1));
        for (index, line) in paragraph.iter_mut().enumerate() {
            line.block_start = index == 0;
        }
    }
}

pub(crate) fn upgrade_cached_page(lines: &[OcrLine], text: &str) -> Option<(Vec<OcrLine>, String, Vec<TranscriptEdit>)> {
    let pages = lines.first()?.page.checked_add(1)?;
    if crate::ocr::recognized_lines(lines, pages).ok()? != text { return None; }
    if let Some(rebuilt) = reconstruct_with_drop_caps(lines) {
        let revised = crate::ocr::recognized_lines(&rebuilt, pages).ok()?;
        if revised != text {
            let edits = layout_edits(text, &revised);
            return Some((rebuilt, revised, edits));
        }
    }
    let mut repaired = lines.to_vec();
    if !numeric_labels::repair(&mut repaired) { return None; }
    let mut offset = 0;
    let mut edits = Vec::new();
    for (index, (before, after)) in lines.iter().zip(&repaired).enumerate() {
        offset += separator_width(index, before);
        let old = before.reading_text();
        let new = after.reading_text();
        if old != new {
            edits.push(TranscriptEdit::between(offset, old, new));
        }
        offset += old.encode_utf16().count();
    }
    let revised = crate::ocr::recognized_lines(&repaired, pages).ok()?;
    Some((repaired, revised, edits))
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

/// Fill holes and recover confirmed initials without replacing raw OCR or manual corrections.
pub(crate) fn supplement_page(
    saved: &[OcrLine], incoming: Vec<OcrLine>,
) -> crate::Result<Option<(Vec<OcrLine>, Vec<TranscriptEdit>)>> {
    let repairs: Vec<_> = saved.iter().map(|line| {
        drop_caps::cached_initial_repair(line, &incoming).map(|(initial, old_bytes)| {
            let old_len = line.text[..old_bytes].encode_utf16().count();
            let new_len = initial.encode_utf16().count();
            (format!("{initial}{}", &line.text[old_bytes..]), old_len, new_len)
        })
    }).collect();
    let mut merged = crate::ocr::merge_missing_lines(saved.to_vec(), incoming);
    let appended = merged.len() != saved.len();
    if !appended && repairs.iter().all(Option::is_none) {
        return Ok(None);
    }
    // Corrections inhibit the generic reorder pass. Their blocks and word
    // coordinates stay attached to the original lines.
    if appended {
        if let Some(reconstructed) = reconstruct_page(&merged) {
            merged = reconstructed;
        }
    }
    let mut edits = Vec::new();
    let mut old_end = 0;
    let mut rows = merged.iter_mut().enumerate();
    for (index, (original, repair)) in saved.iter().zip(repairs).enumerate() {
        let old_gap = separator_width(index, original);
        let mut new_gap = 0;
        let row = loop {
            let (new_index, row) = rows.next()
                .ok_or("Нове OCR змінює порядок збережених рядків.")?;
            new_gap += separator_width(new_index, row);
            if row.text == original.text && row.correction == original.correction
                && row.x == original.x && row.y == original.y
            {
                break row;
            }
            new_gap += row.reading_text().encode_utf16().count();
        };
        if old_gap != new_gap {
            edits.push(TranscriptEdit { start_utf16: old_end, old_len: old_gap, new_len: new_gap });
        }
        if let Some((correction, old_len, new_len)) = repair {
            row.correction = Some(correction);
            edits.push(TranscriptEdit { start_utf16: old_end + old_gap, old_len, new_len });
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
    let rebuilt = match drop_caps::join(lines) {
        Some(joined) => Some(reconstruct_page(&joined).unwrap_or(joined)),
        None => reconstruct_page(lines),
    };
    split_isolated_label_rows(rebuilt.as_deref().unwrap_or(lines)).or(rebuilt)
}

fn split_isolated_label_rows(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    let mut rebuilt = None;
    let mut previous_split = false;
    for (index, line) in lines.iter().enumerate() {
        if let Some(parts) = split_label_row(line) {
            rebuilt.get_or_insert_with(|| lines[..index].to_vec()).extend(parts);
            previous_split = true;
        } else if let Some(rebuilt) = &mut rebuilt {
            let mut line = line.clone();
            line.block_start |= previous_split;
            rebuilt.push(line);
            previous_split = false;
        }
    }
    rebuilt
}

fn split_label_row(line: &OcrLine) -> Option<Vec<OcrLine>> {
    if line.words.len() < 4 { return None; }
    let raw = line.reading_text();
    let mut cuts = crate::ocr::line_gutters(line).peekable();
    cuts.peek()?;
    let mut pieces = Vec::new();
    let mut start = 0;
    let rotation = crate::ocr::line_rotation(line);
    for gutter in cuts {
        let end = gutter.before.raw_start_utf16?.checked_add(gutter.before.text.encode_utf16().count())?;
        let next = gutter.after.raw_start_utf16?;
        let gap = raw.get(byte_at_utf16(raw, end)?..byte_at_utf16(raw, next)?)?;
        if end <= start || gap.is_empty() || !gap.chars().all(char::is_whitespace) { return None; }
        pieces.push(slice_line(line, start, end)?);
        start = next;
    }
    pieces.push(slice_line(line, start, raw.encode_utf16().count())?);
    // Keep a standalone word, an orphan punctuation mark or unanchored ink joined.
    if pieces.iter().any(|piece| piece.words.iter()
        .filter(|word| word.text.chars().any(char::is_alphabetic)).take(2).count() < 2)
        || pieces.iter().map(|piece| piece.words.len()).sum::<usize>() != line.words.len()
        || !pieces.iter().flat_map(|piece| piece.text.split_whitespace()).eq(raw.split_whitespace())
    {
        return None;
    }
    for piece in &mut pieces {
        piece.block_start = true;
        piece.rotation = rotation;
    }
    Some(pieces)
}

fn reconstruct_page(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    if lines.iter().any(|line| line.correction.is_some()) {
        return None;
    }
    let grids = reconstruct_marked_grids(lines);
    let lines = grids.as_deref().unwrap_or(lines);
    let mut ordered = None;
    let mut start = 0;
    for block in lines.chunk_by(|previous, next| !next.block_start &&
        previous.rotation.unwrap_or_default() == next.rotation.unwrap_or_default())
    {
        if let Some(rebuilt) = reconstruct_columns(block, false) {
            ordered.get_or_insert_with(|| lines[..start].to_vec()).extend(rebuilt);
        } else if let Some(ordered) = &mut ordered {
            ordered.extend_from_slice(block);
        }
        start += block.len();
    }
    let starts = ordered.as_deref().unwrap_or(lines).windows(2).enumerate()
        .filter_map(|(index, pair)| {
            (pair[0].rotation != pair[1].rotation && (pair[0].is_rotated() || pair[1].is_rotated())
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
    ordered.or(grids)
}

fn reconstruct_marked_grids(lines: &[OcrLine]) -> Option<Vec<OcrLine>> {
    let mut ordered = None;
    let mut start = 0;
    for run in lines.chunk_by(|first, second| first.rotation.unwrap_or_default() == second.rotation.unwrap_or_default()) {
        let rebuilt = run.iter().skip(1).any(|line| line.block_start)
            .then(|| reconstruct_columns(run, true)).flatten();
        if let Some(rebuilt) = rebuilt {
            ordered.get_or_insert_with(|| lines[..start].to_vec()).extend(rebuilt);
        } else if let Some(ordered) = &mut ordered {
            ordered.extend_from_slice(run);
        }
        start += run.len();
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

fn reconstruct_columns(lines: &[OcrLine], grids_only: bool) -> Option<Vec<OcrLine>> {
    if lines.len() < 2 && !lines.iter().any(|line| line.words.len() >= 4) {
        return None;
    }
    let mut text = String::new();
    let mut spans = Vec::with_capacity(lines.len());
    let mut positioned = Vec::new();
    let mut utf16 = 0;
    let angle = lines.first()?.rotation.unwrap_or_default();
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
            // Only the geometry passed to the detector rotates. The stored word boxes stay on the original page.
            let (x, y, width, height) = crate::ocr::reading_box((word.x, word.y, word.width, word.height), angle);
            let geometry = PositionedWord {
                start: utf16 + left, end: utf16 + right,
                left: x, right: x + width,
                center_y: y + height / 2.0, height, rotation: None,
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
    let edits = if grids_only { separate_confirmed_grids(&text, &positioned) }
        else { separate_blocks(&text, &positioned, 1.0) }.ok()?;
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
                let first = output.len();
                let starts_block = new_block;
                append(lines, &spans, part.start..part.end, &mut new_block, &mut output)?;
                if grids_only { output.get_mut(first)?.block_start = starts_block; }
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
    fn restores_rotated_reading_order_without_joining_an_independent_label() {
        let line = |text: &str, x| OcrLine {
            rotation: Some(90), page: 0, text: text.into(), x, y: 0.1, width: 0.02, height: 0.4,
            confidence: Some(0.98), correction: None, block_start: true,
            source_span: None, words: vec![],
        };
        let mut lines = vec![
            line("les voyageurs du village.", 0.77),
            line("Le marin retrouve", 0.80),
            line("Jardin", 0.74),
        ];
        reconstruct_blocks(&mut lines);
        assert_eq!(crate::ocr::recognized_lines(&lines, 1).unwrap(),
            "Le marin retrouve\nles voyageurs du village.\n\nJardin");
        assert_eq!(lines[0].x, 0.80);
        assert_eq!(lines[1].x, 0.77);
    }

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
    fn joins_decorative_initial_on_either_side_of_an_ocr_apostrophe_break() {
        for (initial, tail) in [("L", "’homme"), ("L’", "homme"), ("L'", "homme")] {
            let mut line = caption_line();
            line.text = format!("{initial} {tail} arrive");
            line.words.truncate(3);
            let tail_start = initial.encode_utf16().count() + 1;
            let next_start = tail_start + tail.encode_utf16().count() + 1;
            for (word, (text, start, x)) in line.words.iter_mut().zip([
                (initial, 0, 0.12), (tail, tail_start, 0.16), ("arrive", next_start, 0.22),
            ]) {
                word.text = text.into();
                word.raw_start_utf16 = Some(start);
                word.x = x;
            }
            line.words[0].width = 0.035;
            let mut regular = vec![line.clone()];
            reconstruct_blocks(&mut regular);
            assert_eq!(regular[0].text, line.text);

            line.words[0].height = 0.045;
            line.words[0].y = 0.295;
            let expected = format!("{initial}{tail} arrive");
            let mut inline = vec![line.clone()];
            reconstruct_blocks(&mut inline);
            crate::ocr::refresh_line_provenance(&expected, &mut inline, None);
            assert_eq!(inline[0].text, expected);
            assert_eq!(inline[0].words[0].x, 0.12);
            assert_eq!(inline[0].words[1].source_span.unwrap().start_utf16, next_start - 1);

            let mut cap = caption_line();
            cap.text = initial.into();
            cap.words.clear();
            cap.x = 0.12;
            cap.y = 0.295;
            cap.width = 0.035;
            cap.height = 0.045;
            line.text = format!("{tail} arrive");
            line.words.remove(0);
            for word in &mut line.words {
                word.raw_start_utf16 = word.raw_start_utf16.map(|start| start - tail_start);
            }
            let mut isolated = vec![line, cap];
            reconstruct_blocks(&mut isolated);
            assert_eq!(isolated.len(), 1);
            assert_eq!(isolated[0].text, expected);
            assert_eq!(isolated[0].words[1].raw_start_utf16, Some(next_start - 1));
        }
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

        for angle in [90, 180, 270] {
            let mut rotated = regular[0].clone();
            rotated.words[0].height = 0.045;
            rotated.words[0].y = 0.295;
            rotated.rotation = Some(angle);
            let original = |x, y, w, h| crate::ocr::reading_box((x, y, w, h), 360 - angle);
            (rotated.x, rotated.y, rotated.width, rotated.height) =
                original(rotated.x, rotated.y, rotated.width, rotated.height);
            for word in &mut rotated.words {
                (word.x, word.y, word.width, word.height) = original(word.x, word.y, word.width, word.height);
            }
            let mut rows = vec![rotated];
            reconstruct_blocks(&mut rows);
            assert_eq!(rows[0].text, "Le bateau", "{angle}°");
            assert_eq!(rows[0].rotation, Some(angle));
        }
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
    fn separates_spatially_independent_labels_before_creating_text_anchors() {
        for angle in [0, 90, 180, 270] {
            let mut line = caption_line();
            let original = |x, y, w, h| crate::ocr::reading_box((x, y, w, h), (360 - angle) % 360);
            (line.x, line.y, line.width, line.height) = original(line.x, line.y, line.width, line.height);
            for word in &mut line.words {
                (word.x, word.y, word.width, word.height) = original(word.x, word.y, word.width, word.height);
            }
            let original_right = (line.words[2].x, line.words[2].y);
            let expected = "Left caption\n\nRight caption";
            let mut rebuilt = vec![line.clone()];
            reconstruct_blocks(&mut rebuilt);
            assert_eq!(crate::ocr::recognized_lines(&rebuilt, 1).unwrap(), expected, "{angle}°");
            crate::ocr::refresh_line_provenance(expected, &mut rebuilt, None);
            assert_eq!(crate::ocr::line_rotation(&rebuilt[1]), Some(angle));
            assert_eq!((rebuilt[1].words[0].x, rebuilt[1].words[0].y), original_right);
            assert_eq!(rebuilt[1].words[0].source_span.unwrap().start_utf16, 14);

            let (_, updated, edits) = upgrade_cached_page(std::slice::from_ref(&line), &line.text).unwrap();
            assert_eq!(updated, expected);
            assert_eq!((edits[0].start_utf16, edits[0].old_len, edits[0].new_len), (12, 1, 2));
            line.correction = Some("Left label Right label".into());
            assert!(upgrade_cached_page(std::slice::from_ref(&line), line.reading_text()).is_none());
        }
    }

    #[test]
    fn re_recognition_repairs_a_cached_initial_without_replacing_raw_ocr() {
        let mut saved = caption_line();
        saved.text = "[L_: 15 février.".into();
        saved.words.truncate(3);
        for (word, (text, offset)) in saved.words.iter_mut().zip([("[L_:", 0), ("15", 5), ("février.", 8)]) {
            word.text = text.into();
            word.raw_start_utf16 = Some(offset);
        }
        saved.words[0].height = 0.05;
        let mut recovered = saved.clone();
        recovered.text = "Le 15 février.".into();
        recovered.words[0].text = "Le".into();
        recovered.words[1].raw_start_utf16 = Some(3);
        recovered.words[2].raw_start_utf16 = Some(6);
        let (lines, edits) = supplement_page(&[saved.clone()], vec![recovered.clone()]).unwrap().unwrap();
        assert_eq!(lines[0].text, saved.text);
        assert_eq!(lines[0].reading_text(), "Le 15 février.");
        assert_eq!((edits[0].start_utf16, edits[0].old_len, edits[0].new_len), (0, 4, 2));
        saved.correction = Some("Le quinze février.".into());
        assert!(supplement_page(&[saved], vec![recovered]).unwrap().is_none());
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
    fn cached_three_column_ocr_recovers_across_previous_block_markers() {
        let row = |words: &[(&str, f64)], y, angle| {
            let mut line = caption_line();
            line.text = words.iter().map(|&(text, _)| text).collect::<Vec<_>>().join(" ");
            line.x = words[0].1;
            line.y = y;
            line.width = words.last().unwrap().1 + 0.05 - line.x;
            line.height = 0.02;
            line.block_start = true;
            line.rotation = (angle != 0).then_some(angle);
            let to_page = |x, y, width, height| crate::ocr::reading_box((x, y, width, height), (360 - angle) % 360);
            (line.x, line.y, line.width, line.height) = to_page(line.x, line.y, line.width, line.height);
            let mut offset = 0;
            line.words = words.iter()
                .map(|&(text, x)| {
                    let (x, y, width, height) = to_page(x, y, 0.05, 0.02);
                    let word = OcrWord { text: text.into(), x, y, width, height,
                        raw_start_utf16: Some(offset), source_span: None };
                    offset += text.encode_utf16().count() + 1;
                    word
                }).collect();
            line
        };
        let full_top = [("Left", 0.05), ("starts", 0.15), ("Middle", 0.46), ("starts", 0.57),
            ("Right", 0.81), ("starts", 0.91)];
        let short_top = [("Left", 0.05), ("Middle", 0.46), ("Right", 0.81)];
        let centered_top = [("Left", 0.1), ("Middle", 0.515), ("Right", 0.86)];
        let body = [("Left", 0.05), ("ends", 0.15), ("Middle", 0.46), ("ends", 0.57), ("Right", 0.81), ("ends", 0.91)];
        let continuation = [("Left", 0.05), ("detail", 0.15), ("Middle", 0.46), ("detail", 0.57),
            ("Right", 0.81), ("detail", 0.91)];
        let root = tempfile::tempdir().unwrap();
        let short_text = "Left\nLeft ends\nLeft detail\n\nMiddle\nMiddle ends\nMiddle detail\n\nRight\nRight ends\nRight detail";
        for (index, (top, top_positions, expected, angle)) in [
            (&full_top[..], [0.05, 0.46, 0.81], "Left starts\nLeft ends\nLeft detail\n\nMiddle starts\nMiddle ends\nMiddle detail\n\nRight starts\nRight ends\nRight detail", 0),
            (&short_top[..], [0.05, 0.46, 0.81], short_text, 0),
            (&centered_top[..], [0.1, 0.515, 0.86], short_text, 0),
            (&centered_top[..], [0.1, 0.515, 0.86], short_text, 90),
            (&centered_top[..], [0.1, 0.515, 0.86], short_text, 180),
            (&centered_top[..], [0.1, 0.515, 0.86], short_text, 270),
        ].into_iter().enumerate() {
            let cached = [row(top, 0.3, angle), row(&body, 0.34, angle), row(&continuation, 0.38, angle)];
            let old = crate::ocr::recognized_lines(&cached, 1).unwrap();
            let (rebuilt, revised, edits) = upgrade_cached_page(&cached, &old).unwrap();
            assert_eq!(revised, expected);
            assert_eq!(rebuilt.iter().map(|line| line.block_start).collect::<Vec<_>>(),
                [false, false, false, true, false, false, true, false, false]);
            assert!(!edits.is_empty());
            assert!(upgrade_cached_page(&rebuilt, &revised).is_none());

            let mut store = crate::Store::open(&root.path().join(format!("reader-{index}.db"))).unwrap();
            let document = store.import_scanned_pdf("grid.pdf", b"%PDF-1.4\n", "en",
                cached.to_vec(), 1).unwrap();
            assert_eq!(document.text, expected);
            let layout = store.pdf_page_translation(&document.id, 0, "uk", true).unwrap();
            let layout = serde_json::to_value(layout).unwrap();
            let columns = layout.as_array().unwrap().iter()
                .map(|row| row["sourceBlockIndex"].as_u64()).collect::<Vec<_>>();
            assert_eq!(columns, [Some(0), Some(1), Some(2)]);
            let geometry = document.ocr_lines.unwrap();
            let positions = top_positions.into_iter().zip([0.05, 0.46, 0.81])
                .flat_map(|(top, body)| [top, body, body]);
            for (line, x) in geometry.iter().zip(positions) {
                assert_eq!(line.rotation.unwrap_or_default(), angle);
                assert!((line.reading_bounds().0 - x).abs() < 1e-9);
                let span = line.source_span.unwrap();
                assert_eq!(&expected[span.start_utf8..span.end_utf8], line.reading_text());
            }
        }
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
