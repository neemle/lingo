use serde::Serialize;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Serialize)]
pub struct Line {
    start: u32,
    end: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextFit {
    size: f32,
    line_height: f32,
    lines: Vec<Line>,
}

struct Word {
    range: Line,
    bytes: Range<usize>,
    starts_line: bool,
}

/// The renderer measures UTF-16 ranges with its own font. An empty range measures line height.
pub fn fit(
    value: &str,
    width: f32,
    height: f32,
    preferred: f32,
    minimum: f32,
    mut measure: impl FnMut(u32, u32, f32) -> f32,
) -> Option<TextFit> {
    if value.len() > 65_536
        || preferred < minimum
        || [width, height, preferred, minimum]
            .into_iter()
            .any(|n| !n.is_finite() || n <= 0.0)
    {
        return None;
    }
    let words = words(value);
    if words.is_empty() { return None; }
    let mut fit_at = |size, allow_token_breaks| {
        let height_sample = measure(0, 0, size);
        if !height_sample.is_finite() || height_sample < 0.0 {
            return None;
        }
        let line_height = (size * 1.16).max(height_sample);
        let max_lines = ((height + 0.01) / line_height).floor() as usize;
        if max_lines == 0 {
            return None;
        }
        let mut fits = |start, end| {
            let measured = measure(start, end - start, size);
            measured.is_finite() && measured >= 0.0 && measured <= width
        };
        let lines = wrap(value, &words, max_lines, allow_token_breaks, &mut fits)?;
        Some(TextFit { size, line_height, lines })
    };
    if let Some(original) = fit_at(preferred, false) {
        return Some(original);
    }
    if preferred == minimum {
        return fit_at(minimum, true);
    }
    let Some(mut best) = fit_at(minimum, false) else {
        // Breaking an oversized token at a grapheme boundary is a last resort.
        return fit_at(minimum, true);
    };
    let mut too_large = preferred;
    for _ in 0..8 {
        let gap = too_large - best.size;
        if gap <= 0.125 {
            break;
        }
        let candidate = best.size + gap * 0.5;
        if let Some(fitted) = fit_at(candidate, false) {
            best = fitted;
        } else {
            too_large = candidate;
        }
    }
    Some(best)
}

fn words(value: &str) -> Vec<Word> {
    let mut words = Vec::<Word>::new();
    let mut offset = 0;
    let mut inside_word = false;
    let mut starts_line = false;
    for (byte, ch) in value.char_indices() {
        let end = offset + ch.len_utf16() as u32;
        if ch.is_whitespace() && !matches!(ch, '\u{a0}' | '\u{2007}' | '\u{202f}') {
            inside_word = false;
            starts_line |= matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}');
        } else if inside_word {
            let word = words.last_mut().expect("active word");
            word.range.end = end;
            word.bytes.end = byte + ch.len_utf8();
        } else {
            words.push(Word {
                range: Line { start: offset, end },
                bytes: byte..byte + ch.len_utf8(),
                starts_line,
            });
            starts_line = false;
            inside_word = true;
        }
        offset = end;
    }
    words
}

fn wrap(
    value: &str,
    words: &[Word],
    max_lines: usize,
    allow_token_breaks: bool,
    fits: &mut impl FnMut(u32, u32) -> bool,
) -> Option<Vec<Line>> {
    let mut lines = Vec::new();
    let mut line: Option<Line> = None;
    for word in words {
        if let Some(current) = line.as_mut() {
            if !word.starts_line && fits(current.start, word.range.end) {
                current.end = word.range.end;
                continue;
            }
            lines.push(*current);
            if lines.len() == max_lines { return None; }
        }
        line = if fits(word.range.start, word.range.end) {
            Some(word.range)
        } else if allow_token_breaks {
            Some(wrap_token(&value[word.bytes.clone()], word.range.start, max_lines, &mut lines, fits)?)
        } else {
            return None;
        };
    }
    lines.push(line?);
    Some(lines)
}

fn wrap_token(
    token: &str,
    offset: u32,
    max_lines: usize,
    lines: &mut Vec<Line>,
    fits: &mut impl FnMut(u32, u32) -> bool,
) -> Option<Line> {
    if token.contains(['\u{a0}', '\u{2007}', '\u{202f}', '\u{2011}', '\u{2060}']) {
        return None;
    }

    let mut ends = vec![offset];
    let mut end = offset;
    for grapheme in token.graphemes(true) {
        end += grapheme.encode_utf16().count() as u32;
        ends.push(end);
    }
    let mut start = 0;
    while start + 1 < ends.len() {
        let count = ends[start + 1..].partition_point(|&end| fits(ends[start], end));
        if count == 0 { return None; }
        let next = start + count;
        let line = Line { start: ends[start], end: ends[next] };
        if next + 1 == ends.len() {
            return Some(line);
        }
        lines.push(line);
        if lines.len() == max_lines { return None; }
        start = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_an_oversized_token_without_splitting_graphemes() {
        let measure = |_, length, size| length as f32 * size;
        let text = "a😀й\u{301}бв";
        let fitted = fit(text, 23.0, 42.0, 10.0, 9.0, measure).unwrap();
        assert_eq!(fitted.size, 9.0);
        assert_eq!(
            fitted.lines.iter().map(|line| (line.start, line.end)).collect::<Vec<_>>(),
            [(0, 1), (1, 3), (3, 5), (5, 7)]
        );
        assert!(fit(text, 23.0, 30.0, 10.0, 9.0, measure).is_none());
        assert!(fit("слово‑слово", 36.0, 80.0, 10.0, 9.0, measure).is_none());
    }

    #[test]
    fn keeps_utf16_ranges_and_nonbreaking_spaces_when_fitting() {
        let measure = |_, length, size| length as f32 * size;
        let fitted = fit("😀 mot !\nsuite", 48.0, 30.0, 8.0, 7.0, measure).unwrap();
        assert_eq!(fitted.size, 8.0);
        assert_eq!(
            fitted
                .lines
                .iter()
                .map(|line| (line.start, line.end))
                .collect::<Vec<_>>(),
            [(0, 2), (3, 8), (9, 14)]
        );
        assert!(fit("mot !", 29.0, 20.0, 8.0, 7.0, measure).is_none());
        assert_eq!(
            fit("mot !", 35.0, 20.0, 8.0, 7.0, measure).unwrap().size,
            7.0
        );

        let resized = fit("une mot deux", 77.0, 26.0, 16.0, 9.0, measure).unwrap();
        assert!((10.875..=11.0).contains(&resized.size));
        assert_eq!(resized.lines.len(), 2);
        assert!(resized.lines.iter().all(|line| measure(line.start, line.end - line.start, resized.size) <= 77.0));
        assert!(resized.line_height * 2.0 <= 26.0);
    }
}
