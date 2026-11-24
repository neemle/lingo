use super::{
    source::{different_color, SourceStyle},
    Bounds,
};
use crate::ocr::{OcrLine, OcrSpan};
use serde::Serialize;
use std::{collections::BTreeMap, ops::Range};

#[derive(Serialize)]
pub(super) struct BackgroundMask {
    bounds: Bounds,
    background: [f64; 4],
}

/// A region with incomplete source geometry stays in the translation list.
pub(super) fn plan(
    area: Bounds,
    spans: impl Iterator<Item = Range<usize>>,
    geometry: &[OcrLine],
    styles: &BTreeMap<usize, &SourceStyle>,
) -> Option<Vec<BackgroundMask>> {
    let mut covered = Vec::<Range<usize>>::new();
    for span in spans.filter(|span| !span.is_empty()) {
        if let Some(last) = covered.last_mut().filter(|last| last.end >= span.start) {
            last.end = last.end.max(span.end);
        } else {
            covered.push(span);
        }
    }
    let overlaps = |span: OcrSpan| {
        covered
            .iter()
            .any(|range| range.start < span.end_utf16 && span.start_utf16 < range.end)
    };
    let contains = |span: OcrSpan| {
        covered
            .iter()
            .any(|range| range.start <= span.start_utf16 && span.end_utf16 <= range.end)
    };
    let background = |span: OcrSpan| styles.get(&span.start_utf16).map(|style| style.background);
    let mask_safe = |span: OcrSpan| styles.get(&span.start_utf16).is_none_or(|style| style.mask_safe);
    let in_area = |x, y, width, height| {
        let (width, height) = area.overlap(Bounds { x, y, width, height });
        width > 0.0001 && height > 0.0001
    };
    let mut masks = Vec::new();
    for line in geometry {
        // A rectangular translation area can span a gap containing unrelated text,
        // such as a caption in a cutout. Check all source lines, not just the rows
        // that have a translation.
        let Some(span) = line.source_span.filter(|&span| overlaps(span)) else {
            let obstructed = if line.words.is_empty() {
                in_area(line.x, line.y, line.width, line.height)
            } else {
                line.words.iter().any(|word| in_area(word.x, word.y, word.width, word.height))
            };
            if obstructed {
                return None;
            }
            continue;
        };
        let line_background = background(span).unwrap_or([1.0; 4]);
        let mixed_background = line
            .words
            .iter()
            .filter_map(|word| word.source_span)
            .any(|span| !mask_safe(span) || background(span).is_some_and(|color| different_color(color, line_background)));
        if contains(span) && mask_safe(span) && !mixed_background && !spans_wide_gaps(line) {
            masks.push(BackgroundMask {
                bounds: clipped(line.x, line.y, line.width, line.height)?,
                background: line_background,
            });
            continue;
        }
        let before = masks.len();
        for word in &line.words {
            let word_span = word.source_span?;
            if !overlaps(word_span) {
                if in_area(word.x, word.y, word.width, word.height) {
                    return None;
                }
                continue;
            }
            if !contains(word_span) || !mask_safe(word_span) {
                return None;
            }
            masks.push(BackgroundMask {
                bounds: clipped(word.x, word.y, word.width, word.height)?,
                background: background(word_span).unwrap_or(line_background),
            });
        }
        if masks.len() == before {
            return None;
        }
    }
    (!masks.is_empty()).then_some(masks)
}

fn spans_wide_gaps(line: &OcrLine) -> bool {
    if line.words.is_empty() {
        return false;
    }
    // OCR can return one line across an icon, a form field, or a column gutter.
    // Those spans of paper are not part of the text to erase.
    let rotation = crate::ocr::line_rotation(line).unwrap_or_default();
    let area = Bounds {
        x: line.x,
        y: line.y,
        width: line.width,
        height: line.height,
    }.reading_box(rotation);
    let gap_limit = area.height * 1.6;
    let mut words: Vec<_> = line.words.iter().map(|word| {
        Bounds { x: word.x, y: word.y, width: word.width, height: word.height }.reading_box(rotation)
    }).collect();
    words.sort_unstable_by(|left, right| left.x.total_cmp(&right.x));
    let mut right_edge = area.x;
    for word in words {
        if word.x - right_edge > gap_limit {
            return true;
        }
        right_edge = right_edge.max(word.right());
    }
    area.right() - right_edge > gap_limit
}

fn clipped(x: f64, y: f64, width: f64, height: f64) -> Option<Bounds> {
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    // Optical glyph bounds can omit the faint outer pixels of an ascender or descender.
    const MARGIN: f64 = 0.0008;
    let left = (x - MARGIN).max(0.0);
    let top = (y - MARGIN).max(0.0);
    let right = (x + width + MARGIN).min(1.0);
    let bottom = (y + height + MARGIN).min(1.0);
    (right > left && bottom > top).then_some(Bounds {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    #[test]
    fn targets_only_covered_words_with_their_own_backgrounds() {
        let span = |start, end| {
            Some(OcrSpan {
                start_utf8: start,
                end_utf8: end,
                start_utf16: start,
                end_utf16: end,
            })
        };
        let word = |start, end, x| OcrWord {
            text: String::new(),
            x,
            y: 0.2,
            width: 0.2,
            height: 0.02,
            raw_start_utf16: None,
            source_span: span(start, end),
        };
        let geometry = [OcrLine {
            rotation: None,
            page: 0,
            text: String::new(),
            x: 0.1,
            y: 0.2,
            width: 0.7,
            height: 0.02,
            confidence: None,
            correction: None,
            block_start: false,
            words: vec![word(0, 4, 0.1), word(5, 8, 0.6)],
            source_span: span(0, 8),
        }];
        let green = SourceStyle::test_style([0.5, 0.9, 0.5], 0.018);
        let styles = BTreeMap::from([(5, &green)]);
        let area = Bounds { x: 0.1, y: 0.2, width: 0.7, height: 0.02 };
        let all = plan(area, std::iter::once(0..8), &geometry, &styles).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].background, [0.5, 0.9, 0.5, 1.0]);
        let right = Bounds { x: 0.6, width: 0.2, ..area };
        let part = plan(right, std::iter::once(5..8), &geometry, &styles).unwrap();
        assert_eq!(part.len(), 1);
        assert!((part[0].bounds.x - 0.6).abs() < 0.001);
        assert!(plan(right, std::iter::once(5..7), &geometry, &styles).is_none());

        let mut unsafe_green = green;
        unsafe_green.mask_safe = false;
        let styles = BTreeMap::from([(5, &unsafe_green)]);
        assert!(plan(area, std::iter::once(0..8), &geometry, &styles).is_none());
        assert!(plan(right, std::iter::once(5..8), &geometry, &styles).is_none());
        let left = Bounds { width: 0.2, ..area };
        assert_eq!(plan(left, std::iter::once(0..4), &geometry, &styles).unwrap().len(), 1);

        let mut compact = geometry[0].clone();
        compact.words[1].x = 0.315;
        compact.width = 0.415;
        let area = Bounds { width: compact.width, ..area };
        let plain = plan(area, std::iter::once(0..8), &[compact], &BTreeMap::new()).unwrap();
        assert_eq!(plain.len(), 1);
    }
}
