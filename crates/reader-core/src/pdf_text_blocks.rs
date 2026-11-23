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

/// Finds page-layout boundaries in UTF-16 text. Geometry uses a top-left coordinate system.
pub fn separate_blocks(text: &str, words: &[PositionedWord], page_width: f64) -> Result<Vec<BlockEdit>> {
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
            lines.push(&remaining[..count]);
        }
        remaining = &remaining[count..];
        line_start = end;
    }

    let mut edits = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if let Some((edit, line_count)) = parallel_lines(&lines[index..], &units, page_width) {
            edits.push(edit);
            index += line_count;
            continue;
        }

        let line = lines[index];
        for split in 2..=line.len().saturating_sub(2) {
            let left = line[split - 1];
            let right = line[split];
            if same_baseline(left, right) && wide_gap(left, right, page_width) &&
                left.end < right.start && whitespace(&units[left.end..right.start])
            {
                edits.push(BlockEdit { start: left.end, end: right.start, slices: Vec::new() });
            }
        }
        if let Some(next) = lines.get(index + 1).filter(|next| next.len() >= 2) {
            if line.len() >= 2 {
                let left = *line.last().expect("line has text");
                let right = next[0];
                let gap = &units[left.end..right.start];
                if whitespace(gap) && gap.iter().filter(|&&unit| unit == 10).count() == 1 &&
                    (same_baseline(left, right) && wide_gap(left, right, page_width) ||
                     column_transition(&lines, index, page_width))
                {
                    edits.push(BlockEdit { start: left.end, end: right.start, slices: Vec::new() });
                }
            }
        }
        index += 1;
    }
    Ok(edits)
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
    let [left_top, right_top, left_bottom, right_bottom, ..] = lines else { return None; };
    if [left_top, right_top, left_bottom, right_bottom].iter().any(|line| line.len() < 2) {
        return None;
    }
    let left = left_top[0];
    let left_next = left_bottom[0];
    let right_next = right_bottom[0];
    if !aligned(left, left_next) || !same_baseline(left_next, right_next) ||
        !wide_gap(*left_bottom.last()?, right_next, page_width) {
        return None;
    }
    let mut right = right_top[0];
    let mut middle = None;
    if !aligned(right, right_next) && right_top.len() >= 3 {
        let next = right_top[1];
        if right.right - right.left <= 2.5 * next.height &&
            right.left > left_top.last()?.right && next.left - right.right >= 1.25 * next.height {
            middle = Some(right);
            right = next;
        }
    }

    let next_line = |upper: PositionedWord, lower: PositionedWord| {
        let drop = lower.center_y - upper.center_y;
        (left.height * 0.6..=left.height * 2.2).contains(&drop)
    };
    if !next_line(left, left_next) || !aligned(right, right_next) ||
        !same_baseline(left, right) || !wide_gap(*left_top.last()?, right, page_width) {
        return None;
    }

    let piece = |first: PositionedWord, last: PositionedWord, separator| SourcePiece {
        start: first.start, end: last.end, separator,
    };
    let mut left_pieces = vec![piece(left, *left_top.last()?, "")];
    let mut right_pieces = vec![piece(right, *right_top.last()?, "\n\n")];
    let mut line_count = 2;
    let mut previous_left = left;
    for pair in lines[2..].chunks_exact(2).take(23) {
        let [next_left, next_right] = pair else { break; };
        if next_left.len() < 2 || next_right.len() < 2 ||
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
    if line_count < 4 { return None; }
    let start = left.start;
    let end = right_pieces.last()?.end;
    if end - start > 2048 { return None; }
    if let Some(label) = middle {
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

fn whitespace(text: &[u16]) -> bool {
    text.iter().all(|unit| char::from_u32(u32::from(*unit)).is_some_and(char::is_whitespace))
}

fn wide_gap(left: PositionedWord, right: PositionedWord, page_width: f64) -> bool {
    right.left - left.right >= (page_width * 0.085).max(3.5 * left.height.max(right.height))
}

fn same_baseline(left: PositionedWord, right: PositionedWord) -> bool {
    (left.center_y - right.center_y).abs() < 0.55 * left.height.min(right.height)
}

fn aligned(upper: PositionedWord, lower: PositionedWord) -> bool {
    (upper.left - lower.left).abs() <= upper.height.min(lower.height) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

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
                left, right: left + width, center_y, height: 10.0,
            }
        });
        let edits = separate_blocks(text, &words, 420.0).unwrap();
        assert_eq!(edits.len(), 2);
        let units = text.encode_utf16().collect::<Vec<_>>();
        let reconstructed = edits[0].slices.iter().map(|part| format!(
            "{}{}", part.separator, String::from_utf16(&units[part.start..part.end]).unwrap(),
        )).collect::<String>();
        assert_eq!(reconstructed, "𐐷 One left\nSecond left\n\nOR\n\nOne right\nSecond right");
        assert_eq!(edits[1].end - edits[1].start, 1);
        assert!(edits[1].slices.is_empty());
    }
}
