use crate::Result;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PositionedWord {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) left: f64,
    pub(crate) right: f64,
    pub(crate) center_y: f64,
    pub(crate) height: f64,
    #[serde(default)]
    pub(crate) rotation: Option<u16>,
}

#[derive(Serialize)]
pub struct SourcePiece {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) separator: &'static str,
}

#[derive(Serialize)]
pub struct BlockEdit {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) slices: Vec<SourcePiece>,
}

#[derive(Serialize)]
pub struct LigatureJoin {
    start: usize,
    end: usize,
}

pub(crate) fn ligature_boundary(left: &str, gap: &str, right: &str) -> bool {
    (left.ends_with("fi") || left.ends_with("fl")) &&
        !gap.is_empty() && gap.chars().all(|ch| matches!(ch, ' ' | '\t' | '\u{00a0}' | '\u{202f}')) &&
        right.starts_with(char::is_lowercase)
}

/// PDF extraction sometimes puts a space after a ligature even when the ink is contiguous.
pub fn ligature_joins(text: &str, words: &[PositionedWord]) -> Result<Vec<LigatureJoin>> {
    let units = positioned_utf16(text, words, 1.0)?;
    Ok(words.windows(2).filter_map(|pair| {
        let [left, right] = pair else { return None; };
        let height = left.height.min(right.height);
        let horizontal = right.left - left.right;
        if right.start <= left.end || left.height.max(right.height) > 1.25 * height ||
            (left.center_y - right.center_y).abs() > 0.12 * height ||
            !(-0.06 * height..=0.04 * height).contains(&horizontal) {
            return None;
        }
        let spelling = String::from_utf16(&units[left.start..left.end]).ok()?;
        let gap = String::from_utf16(&units[left.end..right.start]).ok()?;
        let continuation = String::from_utf16(&units[right.start..right.end]).ok()?;
        ligature_boundary(&spelling, &gap, &continuation)
            .then_some(LigatureJoin { start: left.end, end: right.start })
    }).collect())
}

/// Finds page-layout boundaries in UTF-16 text. Geometry uses a top-left coordinate system.
pub fn separate_blocks(text: &str, words: &[PositionedWord], page_width: f64) -> Result<Vec<BlockEdit>> {
    let units = positioned_utf16(text, words, page_width)?;
    let mut edits = Vec::new();
    let (mut word_start, mut text_start) = (0, 0);
    let mut direction = None;
    for (index, word) in words.iter().enumerate() {
        let Some(rotation @ (0 | 90 | 180 | 270)) = word.rotation else { continue; };
        let run_rotation = *direction.get_or_insert(rotation);
        if run_rotation == rotation {
            continue;
        }
        let previous_end = words[index - 1].end;
        let gap = &units[previous_end..word.start];
        if gap.is_empty() || !whitespace(gap) {
            continue;
        }
        if run_rotation == 0 {
            edits.extend(horizontal_run(&units, &words[word_start..index], page_width, text_start, previous_end)?);
        }
        if line_break_count(gap) < 2 {
            edits.push(BlockEdit { start: previous_end, end: word.start, slices: Vec::new() });
        }
        (word_start, text_start, direction) = (index, word.start, Some(rotation));
    }
    if direction.unwrap_or_default() == 0 {
        edits.extend(horizontal_run(&units, &words[word_start..], page_width, text_start, units.len())?);
    }
    Ok(edits)
}

fn horizontal_run(
    units: &[u16], words: &[PositionedWord], page_width: f64, start: usize, end: usize,
) -> Result<Vec<BlockEdit>> {
    if start == 0 {
        return separate_positioned_blocks(&units[..end], words, page_width);
    }
    let relative = words.iter().map(|word| PositionedWord {
        start: word.start - start, end: word.end - start, ..*word
    }).collect::<Vec<_>>();
    let mut edits = separate_positioned_blocks(&units[start..end], &relative, page_width)?;
    for edit in &mut edits {
        edit.start += start;
        edit.end += start;
        for slice in &mut edit.slices {
            slice.start += start;
            slice.end += start;
        }
    }
    Ok(edits)
}

fn positioned_utf16(text: &str, words: &[PositionedWord], page_width: f64) -> Result<Vec<u16>> {
    if text.len() > 96_000 || words.len() > 24_000 || !page_width.is_finite() || page_width <= 0.0 {
        return Err("Некоректна геометрія текстових блоків PDF.".into());
    }
    let units = text.encode_utf16().collect::<Vec<_>>();
    if units.len() > 24_000 {
        return Err("Текст сторінки PDF завеликий для визначення блоків.".into());
    }
    let mut previous_end = 0;
    for word in words {
        if word.start < previous_end || word.end <= word.start || word.end > units.len() ||
            ![word.left, word.right, word.center_y, word.height].into_iter().all(f64::is_finite) ||
            word.right <= word.left || word.height <= 0.0 ||
            [word.start, word.end].into_iter().any(|index|
                units.get(index).is_some_and(|unit| (0xdc00..=0xdfff).contains(unit)))
        {
            return Err("Некоректні координати слова PDF.".into());
        }
        previous_end = word.end;
    }
    Ok(units)
}

fn separate_positioned_blocks(units: &[u16], words: &[PositionedWord], page_width: f64) -> Result<Vec<BlockEdit>> {
    let mut line_start = 0;
    let mut remaining = words;
    let mut lines = Vec::new();
    for line in units.split_inclusive(|unit| *unit == 10 || *unit == 13) {
        let end = line_start + line.len();
        let has_text = line.iter().any(|unit| *unit != 10 && *unit != 13);
        let count = remaining.partition_point(|word| word.start < end);
        if let Some(word) = remaining[..count].last() {
            if word.end > end || units[word.start..word.end].contains(&10) || units[word.start..word.end].contains(&13) {
                return Err("Координати слова перетинають рядки PDF.".into());
            }
        }
        if has_text {
            let row = &remaining[..count];
            let mut start = 0;
            for split in 1..row.len() {
                let left = row[split - 1];
                let right = row[split];
                if left.end < right.start &&
                    same_baseline(left, right) && wide_gap(left, right, page_width) &&
                    whitespace(&units[left.end..right.start])
                {
                    lines.push(&row[start..split]);
                    start = split;
                }
            }
            lines.push(&row[start..]);
        }
        remaining = &remaining[count..];
        line_start = end;
    }

    let mut edits = Vec::new();
    let mut surroundings = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if let Some((edit, line_count)) = parallel_lines(&lines[index..], units, page_width) {
            isolate_block(units, edit.start, edit.end, &mut surroundings);
            edits.push(edit);
            index += line_count;
            continue;
        }

        let line = lines[index];
        if let Some(next) = lines.get(index + 1).filter(|next| next.len() >= 2) {
            if line.len() >= 2 {
                let left = *line.last().expect("line has text");
                let right = next[0];
                let gap = &units[left.end..right.start];
                let newlines = line_break_count(gap);
                let parallel = newlines <= 1 && same_baseline(left, right) && wide_gap(left, right, page_width);
                if !gap.is_empty() && whitespace(gap) &&
                    (parallel ||
                     newlines == 1 && column_transition(&lines, index, page_width))
                {
                    edits.push(BlockEdit { start: left.end, end: right.start, slices: Vec::new() });
                    if parallel {
                        isolate_block(units, line[0].start, next.last().unwrap().end, &mut surroundings);
                    }
                }
            }
        }
        index += 1;
    }
    edits.extend(surroundings);
    edits.sort_by_key(|edit| (edit.start, edit.end));
    edits.dedup_by_key(|edit| (edit.start, edit.end));
    Ok(edits)
}

fn isolate_block(units: &[u16], start: usize, end: usize, edits: &mut Vec<BlockEdit>) {
    let before = units[..start].iter().rposition(|unit| !space(*unit)).map_or(0, |index| index + 1);
    let after = units[end..].iter().position(|unit| !space(*unit)).map_or(units.len(), |index| end + index);
    for (left, right) in [(before, start), (end, after)] {
        let gap = &units[left..right];
        if left > 0 && right < units.len() && line_break_count(gap) == 1 {
            edits.push(BlockEdit { start: left, end: right, slices: Vec::new() });
        }
    }
}

fn line_break_count(gap: &[u16]) -> usize {
    gap.iter().enumerate().filter(|&(index, &unit)|
        unit == 13 || unit == 10 && (index == 0 || gap[index - 1] != 13)).count()
}

// Some OCR providers already emit all lines in the left column before the right column.
fn column_transition(lines: &[&[PositionedWord]], index: usize, width: f64) -> bool {
    let left_end = lines[index];
    let right_start = lines[index + 1];
    if right_start[0].center_y >= left_end[0].center_y ||
        !wide_gap(*left_end.last().expect("nonempty column"), right_start[0], width) {
        return false;
    }

    let next_line = |upper: PositionedWord, lower: PositionedWord| {
        let step = lower.center_y - upper.center_y;
        step > 0.0 && step <= 2.5 * upper.height.max(lower.height)
    };
    let mut lower = left_end[0];
    let mut has_top = false;
    for line in lines[..index].iter().rev().take(23) {
        if line.len() < 2 || !aligned(left_end[0], line[0]) || !next_line(line[0], lower) { break; }
        if same_baseline(line[0], right_start[0]) &&
            wide_gap(*line.last().expect("nonempty column"), right_start[0], width) {
            has_top = true;
            break;
        }
        lower = line[0];
    }
    if !has_top { return false; }
    let mut upper = right_start[0];
    for line in lines[index + 2..].iter().take(23) {
        if line.len() < 2 || !aligned(right_start[0], line[0]) || !next_line(upper, line[0]) { break; }
        if same_baseline(left_end[0], line[0]) &&
            wide_gap(*left_end.last().expect("nonempty column"), line[0], width) {
            return true;
        }
        upper = line[0];
    }
    false
}

fn parallel_lines(lines: &[&[PositionedWord]], units: &[u16], page_width: f64) -> Option<(BlockEdit, usize)> {
    let [left_top, second, remaining @ ..] = lines else { return None; };
    let mut left_top = *left_top;
    let (mut right_top, mut label, following) = match (*second, remaining) {
        ([label], [right_top, following @ ..]) => (*right_top, Some(*label), following),
        _ => (*second, None, remaining),
    };
    let [left_bottom, right_bottom, ..] = following else { return None; };
    if left_top.len() < 2 || right_top.len() < 2 || !column_continuation(left_bottom, right_bottom) {
        return None;
    }
    let left = left_top[0];
    let left_next = left_bottom[0];
    let right_next = right_bottom[0];
    if !aligned(left, left_next) || !same_baseline(left_next, right_next) ||
        !wide_gap(*left_bottom.last()?, right_next, page_width) {
        return None;
    }
    if label.is_none() {
        if let [column @ .., last] = left_top {
            if column.len() >= 2 && gutter_label(*column.last()?, *last, right_top[0]) {
                left_top = column;
                label = Some(*last);
            }
        }
    }
    if label.is_none() && !aligned(right_top[0], right_next) {
        if let [first, column @ ..] = right_top {
            if column.len() >= 2 && gutter_label(*left_top.last()?, *first, column[0]) {
                right_top = column;
                label = Some(*first);
            }
        }
    }

    let right = right_top[0];
    let left_end = *left_top.last()?;
    let next_line = |upper: PositionedWord, lower: PositionedWord| {
        let drop = lower.center_y - upper.center_y;
        (upper.height.min(lower.height) * 0.6..=upper.height.max(lower.height) * 2.5)
            .contains(&drop)
    };
    if !next_line(left, left_next) || !aligned(right, right_next) ||
        !same_baseline(left, right) || !wide_gap(left_end, right, page_width) ||
        label.is_some_and(|label| !gutter_label(left_end, label, right) ||
            label.center_y < left.center_y - 0.5 * left.height ||
            label.center_y > left_next.center_y + 0.5 * left_next.height) {
        return None;
    }

    let piece = |first: PositionedWord, last: PositionedWord, separator| SourcePiece {
        start: first.start, end: last.end, separator,
    };
    let mut left_pieces = vec![piece(left, left_end, "")];
    let mut right_pieces = vec![piece(right, *right_top.last()?, "\n\n")];
    let mut line_count = lines.len() - following.len();
    let mut previous_left = left;
    for pair in following.chunks_exact(2).take(23) {
        let [next_left, next_right] = pair else { break; };
        if !column_continuation(next_left, next_right) ||
            !aligned(left, next_left[0]) || !aligned(right, next_right[0]) ||
            !next_line(previous_left, next_left[0]) ||
            !same_baseline(next_left[0], next_right[0]) ||
            !wide_gap(*next_left.last()?, next_right[0], page_width) {
            break;
        }
        left_pieces.push(piece(next_left[0], *next_left.last()?, "\n"));
        right_pieces.push(piece(next_right[0], *next_right.last()?, "\n"));
        previous_left = next_left[0];
        line_count += 2;
    }
    if left_pieces.len() < 2 { return None; }
    let start = left.start;
    let end = right_pieces.last()?.end;
    if end - start > 2048 { return None; }
    if let Some(label) = label {
        left_pieces.push(piece(label, label, "\n\n"));
    }
    left_pieces.extend(right_pieces);
    let mut source_order = left_pieces.iter().collect::<Vec<_>>();
    source_order.sort_unstable_by_key(|part| part.start);
    let mut cursor = start;
    for part in source_order {
        if part.start < cursor || !whitespace(&units[cursor..part.start]) { return None; }
        cursor = part.end;
    }
    Some((BlockEdit { start, end, slices: left_pieces }, line_count))
}

// A confirmed pair of text columns can end in a single-word wrapped line.
fn column_continuation(left: &[PositionedWord], right: &[PositionedWord]) -> bool {
    !left.is_empty() && !right.is_empty() && (left.len() >= 2 || right.len() >= 2)
}

fn gutter_label(left: PositionedWord, label: PositionedWord, right: PositionedWord) -> bool {
    let character_width = average_character_width(left).max(average_character_width(right));
    label.right - label.left <= 5.0 * character_width &&
        label.left - left.right >= 2.5 * character_width &&
        right.left - label.right >= 2.5 * character_width
}

fn whitespace(text: &[u16]) -> bool {
    text.iter().all(|&unit| space(unit))
}

fn space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(char::is_whitespace)
}

fn wide_gap(left: PositionedWord, right: PositionedWord, page_width: f64) -> bool {
    let character_width = average_character_width(left).max(average_character_width(right));
    right.left - left.right >= (page_width * 0.085).max(7.0 * character_width)
}

fn same_baseline(left: PositionedWord, right: PositionedWord) -> bool {
    (left.center_y - right.center_y).abs() < 0.55 * left.height.min(right.height)
}

fn aligned(upper: PositionedWord, lower: PositionedWord) -> bool {
    (upper.left - lower.left).abs() <= average_character_width(upper).min(average_character_width(lower))
}

// OCR can normalize the axes independently. Compare distances on the same axis.
fn average_character_width(word: PositionedWord) -> f64 {
    (word.right - word.left) / (word.end - word.start) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_rotated_captions_and_keeps_later_column_ranges_absolute() {
        let text = "𐐷 Body\r\nCaption\r\n\r\nLeft starts Right starts\nLeft ends Right ends";
        let words = text.split_whitespace()
            .zip([10.0, 24.0, 94.0, 10.0, 24.0, 70.0, 85.0, 10.0, 24.0, 70.0, 85.0])
            .enumerate().scan(0, |cursor, (index, (token, left))| {
                let start = *cursor + text[*cursor..].find(token).unwrap();
                *cursor = start + token.len();
                Some(PositionedWord {
                    start: text[..start].encode_utf16().count(),
                    end: text[..*cursor].encode_utf16().count(),
                    left, right: left + 8.0, center_y: if index < 7 { 10.0 } else { 19.0 },
                    height: 4.0, rotation: Some(if index == 2 { 90 } else { 0 }),
                })
            }).collect::<Vec<_>>();
        let edits = separate_blocks(text, &words, 100.0).unwrap();
        let units = text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(edits.len(), 2);
        assert_eq!(&units[edits[0].start..edits[0].end], &[13, 10]);
        assert!(edits[0].slices.is_empty());
        let layout = &edits[1];
        assert_eq!(layout.start, words[3].start);
        let output = layout.slices.iter().map(|part| format!("{}{}", part.separator,
            String::from_utf16(&units[part.start..part.end]).unwrap())).collect::<String>();
        assert_eq!(output, "Left starts\nLeft ends\n\nRight starts\nRight ends");
    }

    #[test]
    fn joins_ligature_gaps_without_touching_real_word_spaces() {
        let text = "𐐷 vérifi ez défi réel";
        let tokens = [("vérifi", 0.0, 30.0), ("ez", 30.01, 38.0),
            ("défi", 45.0, 62.0), ("réel", 66.0, 80.0)];
        let words = tokens.map(|(token, left, right)| {
            let index = text.find(token).unwrap();
            let start = text[..index].encode_utf16().count();
            PositionedWord { start, end: start + token.encode_utf16().count(), left, right,
                center_y: 10.0, height: 14.0, rotation: None }
        });
        let joins = ligature_joins(text, &words).unwrap();
        assert_eq!(joins.len(), 1);
        assert_eq!((joins[0].start, joins[0].end), (9, 10));
    }

    #[test]
    fn separates_columns_when_ocr_groups_two_columns_into_each_row() {
        let text = "Left starts Right starts\nLeft ends Right ends";
        let words = text.split_whitespace().zip([10.0, 24.0, 70.0, 85.0, 10.0, 24.0, 70.0, 85.0])
            .enumerate().scan(0, |cursor, (index, (word, left))| {
                let start = *cursor + text[*cursor..].find(word).unwrap();
                let end = start + word.len();
                *cursor = end;
                Some(PositionedWord {
                    start, end, left, right: left + 8.0,
                    center_y: if index < 4 { 10.0 } else { 19.0 },
                    height: if index == 4 { 3.0 } else { 4.0 },
                    rotation: None,
                })
            }).collect::<Vec<_>>();
        // The same page can also arrive with each OCR axis independently normalized.
        let normalized = words.iter().map(|word| PositionedWord {
            left: word.left / 100.0,
            right: word.right / 100.0,
            center_y: word.center_y / 25.0,
            height: word.height / 25.0,
            ..*word
        }).collect::<Vec<_>>();
        for (geometry, width) in [(&words, 100.0), (&normalized, 1.0)] {
            let edits = separate_blocks(text, geometry, width).unwrap();
            assert_eq!(edits.len(), 1);
            let reordered = edits[0].slices.iter()
                .map(|part| format!("{}{}", part.separator, &text[part.start..part.end])).collect::<String>();
            assert_eq!(reordered, "Left starts\nLeft ends\n\nRight starts\nRight ends");
        }
    }

    #[test]
    fn keeps_short_wrapped_lines_in_their_columns() {
        let text = "Left starts Right starts\r\nword Right continues\r\nLeft continues end\r\nFooter text";
        let coordinates = [
            (12.0, 10.0), (30.0, 10.0), (145.0, 10.0), (175.0, 10.0),
            (12.0, 24.0), (145.0, 24.0), (175.0, 24.0),
            (12.0, 38.0), (30.0, 38.0), (145.0, 38.0),
            (12.0, 80.0), (42.0, 80.0),
        ];
        let mut cursor = 0;
        let words = text.split_whitespace().zip(coordinates).map(|(token, (left, center_y))| {
            let start = cursor + text[cursor..].find(token).unwrap();
            cursor = start + token.len();
            PositionedWord { start, end: cursor, left, right: left + 3.0 * token.len() as f64,
                center_y, height: 10.0, rotation: None }
        }).collect::<Vec<_>>();
        let edits = separate_blocks(text, &words, 240.0).unwrap();
        let output = edits[0].slices.iter().map(|piece|
            format!("{}{}", piece.separator, &text[piece.start..piece.end])).collect::<String>();
        assert_eq!(output, "Left starts\nword\nLeft continues\n\nRight starts\nRight continues\nend");
        assert_eq!(&text[edits[0].end..], "\r\nFooter text");

        // A single pair of widely spaced words has no corroborating column geometry.
        let lone_row = "word Right";
        let single_pair = [
            PositionedWord { start: 0, end: 4, ..words[4] },
            PositionedWord { start: 5, end: 10, ..words[5] },
        ];
        assert!(separate_blocks(lone_row, &single_pair, 240.0).unwrap().is_empty());
    }

    #[test]
    fn separates_parallel_blocks_and_keeps_original_utf16_spans() {
        let text = "𐐷 One left\nOR One right\nSecond left\nSecond right\nLeft caption Right caption";
        let spans = [
            ("𐐷", 75.0, 13.0, 10.0), ("One", 92.0, 21.0, 10.0), ("left", 115.0, 20.0, 10.0),
            ("OR", 215.0, 19.0, 11.0), ("One", 290.0, 21.0, 11.0), ("right", 314.0, 28.0, 11.0),
            ("Second", 75.0, 35.0, 23.0), ("left", 115.0, 20.0, 23.0),
            ("Second", 290.0, 35.0, 23.0), ("right", 329.0, 28.0, 23.0),
            ("Left", 80.0, 20.0, 60.0), ("caption", 105.0, 34.0, 60.0),
            ("Right", 292.0, 22.0, 60.0), ("caption", 321.0, 33.0, 60.0),
        ];
        let mut offset = 0;
        let words = spans.map(|(token, left, width, center_y)| {
            let start = offset + text[offset..].find(token).unwrap();
            offset = start + token.len();
            PositionedWord {
                start: text[..start].encode_utf16().count(),
                end: text[..offset].encode_utf16().count(),
                left, right: left + width, center_y, height: 10.0, rotation: None,
            }
        });
        let edits = separate_blocks(text, &words, 420.0).unwrap();
        assert_eq!(edits.len(), 3);
        let units = text.encode_utf16().collect::<Vec<_>>();
        let reconstructed = edits[0].slices.iter().map(|part| format!(
            "{}{}", part.separator, String::from_utf16(&units[part.start..part.end]).unwrap(),
        )).collect::<String>();
        assert_eq!(reconstructed, "𐐷 One left\nSecond left\n\nOR\n\nOne right\nSecond right");
        assert_eq!(edits[1].start, edits[0].end);
        assert_eq!(edits[2].end - edits[2].start, 1);
        assert!(edits[2].slices.is_empty());
    }

    #[test]
    fn keeps_center_label_separate_when_ocr_places_it_on_either_side_or_its_own_row() {
        for top in ["Left option OU\nRight option", "Left option\nOU Right option",
            "Left option\nOU\nRight option", "Left option OU Right option"] {
            let text = format!("{top}\nLeft detail\nRight detail\nSubsequent heading");
            let positions = [30.0, 62.0, 152.0, 222.0, 257.0, 30.0, 62.0, 222.0, 257.0, 30.0, 85.0];
            let mut cursor = 0;
            let words = text.split_whitespace().zip(positions).enumerate()
                .map(|(index, (word, left))| {
                    let start = cursor + text[cursor..].find(word).unwrap();
                    cursor = start + word.len();
                    PositionedWord {
                        start, end: cursor, left, right: left + 5.0 * word.len() as f64,
                        center_y: 20.0 + 14.0 * (if index < 5 { 0 } else if index < 9 { 1 } else { 3 }) as f64,
                        height: 10.0,
                        rotation: None,
                    }
                }).collect::<Vec<_>>();
            let edits = separate_blocks(&text, &words, 320.0).unwrap();
            assert_eq!(edits.len(), 2, "{top}");
            let edit = &edits[0];
            let reordered = edit.slices.iter()
                .map(|part| format!("{}{}", part.separator, &text[part.start..part.end])).collect::<String>();
            assert_eq!(reordered, "Left option\nLeft detail\n\nOU\n\nRight option\nRight detail", "{top}");
            assert_eq!(&text[edit.end..], "\nSubsequent heading");
            assert_eq!(edits[1].start..edits[1].end, edit.end..edit.end + 1);
            assert_eq!(edit.slices[2].start, text.find("OU").unwrap());
        }
    }
}
