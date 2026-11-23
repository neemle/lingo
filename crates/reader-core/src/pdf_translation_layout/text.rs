use serde::Serialize;

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
    let first = words.first()?.range;
    let mut size = preferred;
    loop {
        let height_sample = measure(0, 0, size);
        if !height_sample.is_finite() || height_sample < 0.0 {
            return None;
        }
        let line_height = (size * 1.16).max(height_sample);
        let lines = wrap(&words[1..], first, width, size, &mut measure);
        if let Some(lines) = lines.filter(|lines| line_height * lines.len() as f32 <= height + 0.01)
        {
            return Some(TextFit {
                size,
                line_height,
                lines,
            });
        }
        if size <= minimum {
            return None;
        }
        let smaller = (size * 0.84).max(minimum);
        if smaller >= size {
            return None;
        }
        size = smaller;
    }
}

fn words(value: &str) -> Vec<Word> {
    let mut words = Vec::<Word>::new();
    let mut offset = 0;
    let mut inside_word = false;
    let mut starts_line = false;
    for ch in value.chars() {
        let end = offset + ch.len_utf16() as u32;
        if ch.is_whitespace() && !matches!(ch, '\u{a0}' | '\u{2007}' | '\u{202f}') {
            inside_word = false;
            starts_line |= matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}');
        } else if inside_word {
            words.last_mut().expect("active word").range.end = end;
        } else {
            words.push(Word {
                range: Line { start: offset, end },
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
    remaining: &[Word],
    mut line: Line,
    width: f32,
    size: f32,
    measure: &mut impl FnMut(u32, u32, f32) -> f32,
) -> Option<Vec<Line>> {
    let mut fits = |start, end| {
        let measured = measure(start, end - start, size);
        measured.is_finite() && measured >= 0.0 && measured <= width
    };
    if !fits(line.start, line.end) {
        return None;
    }
    let mut lines = Vec::new();
    for word in remaining {
        if !word.starts_line && fits(line.start, word.range.end) {
            line.end = word.range.end;
        } else {
            lines.push(line);
            line = word.range;
            if !fits(line.start, line.end) {
                return None;
            }
        }
    }
    lines.push(line);
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
