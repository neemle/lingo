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
    let mut masks = Vec::new();
    for line in geometry {
        let Some(span) = line.source_span.filter(|&span| overlaps(span)) else {
            continue;
        };
        let line_background = background(span).unwrap_or([1.0; 4]);
        let mixed_background = line
            .words
            .iter()
            .filter_map(|word| word.source_span.and_then(background))
            .any(|color| different_color(color, line_background));
        if contains(span) && !mixed_background {
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
                continue;
            }
            if !contains(word_span) {
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

fn clipped(x: f64, y: f64, width: f64, height: f64) -> Option<Bounds> {
    let left = x.max(0.0);
    let top = y.max(0.0);
    let right = (x + width).min(1.0);
    let bottom = (y + height).min(1.0);
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
        let all = plan(std::iter::once(0..8), &geometry, &styles).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[1].background, [0.5, 0.9, 0.5, 1.0]);
        let part = plan(std::iter::once(5..8), &geometry, &styles).unwrap();
        assert_eq!(part.len(), 1);
        assert_eq!(part[0].bounds.x, 0.6);
        assert!(plan(std::iter::once(5..7), &geometry, &styles).is_none());
    }
}
