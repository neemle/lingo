use crate::ocr::{OcrLine, OcrWord, byte_at_utf16, line_rotation, reading_box};

struct Label {
    number: u16,
    rotation: u16,
    x: f64,
    y: f64,
    body_x: f64,
    digit_width: f64,
    body_start: usize,
    fragmented: bool,
}

pub(super) fn repair(page: &mut [OcrLine]) -> bool {
    let labels: Vec<_> = page
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let label = label(line)?;
            begins_lettered_list(&label, &page[index + 1..]).then_some((index, label))
        })
        .collect();
    let mut corroborated = vec![false; labels.len()];
    for (index, pair) in labels.windows(2).enumerate() {
        let [(_, first), (_, second)] = pair else {
            continue;
        };
        let width = first.digit_width.max(second.digit_width);
        if first.number + 1 == second.number
            && first.rotation == second.rotation
            && first.y < second.y
            && (first.x - second.x).abs() <= width
            && (first.body_x - second.body_x).abs() <= width
        {
            corroborated[index..index + 2].fill(true);
        }
    }

    let mut changed = false;
    for ((index, label), confirmed) in labels.into_iter().zip(corroborated) {
        let line = &mut page[index];
        if confirmed && label.fragmented && line.correction.is_none() {
            line.correction = Some(format!(
                "{}. {}",
                label.number,
                &line.text.trim()[label.body_start..]
            ));
            changed = true;
        }
    }
    changed
}

fn label(line: &OcrLine) -> Option<Label> {
    if !line
        .text
        .trim_start()
        .starts_with(|ch: char| ch.is_ascii_digit())
    {
        return None;
    }
    let rotation = line_rotation(line).unwrap_or_default();
    let first = line.words.first()?;
    let (x, y, width, height) = word_box(first, rotation);
    let mut right = x;
    let mut digits = 0;
    let mut number = 0u16;
    for (index, word) in line.words.iter().take(4).enumerate() {
        let (wx, wy, ww, wh) = word_box(word, rotation);
        let gap = wx - right;
        if !(-0.15 * width..=0.7 * width).contains(&gap) {
            return None;
        }
        let decimal = word.text.strip_suffix('.');
        let chunk = decimal.unwrap_or(&word.text);
        // A dot misread as a plus sits on the baseline and is much smaller than a digit.
        let small_punctuation = matches!(word.text.as_str(), "." | "+")
            && wh <= 0.45 * height
            && wy >= y + 0.6 * height
            && wy + wh <= y + 1.15 * height;
        if !small_punctuation {
            if chunk.is_empty()
                || digits + chunk.len() > 3
                || !chunk.bytes().all(|byte| byte.is_ascii_digit())
                || (index > 0 && chunk.len() != 1)
                || (wy - y).abs() > 0.3 * height
                || !(0.7 * height..=1.3 * height).contains(&wh)
                || (digits == 0 && chunk.starts_with('0'))
            {
                return None;
            }
            for digit in chunk.bytes() {
                number = number * 10 + u16::from(digit - b'0');
            }
            digits += chunk.len();
        }
        right = wx + ww;
        if decimal.is_some() || small_punctuation {
            if digits == 0 || (index > 0 && digits < 2) {
                return None;
            }
            let next = line.words.get(index + 1)?;
            let (body_x, _, _, _) = word_box(next, rotation);
            if !next.text.starts_with(char::is_uppercase)
                || !(0.2 * width..=2.0 * width).contains(&(body_x - right))
            {
                return None;
            }
            let raw = line.text.trim();
            let body_start = byte_at_utf16(raw, next.raw_start_utf16?)?;
            if !raw[body_start..].starts_with(&next.text)
                || !raw[..body_start]
                    .split_whitespace()
                    .eq(line.words[..=index].iter().map(|word| word.text.as_str()))
            {
                return None;
            }
            return Some(Label {
                number,
                rotation,
                x,
                y,
                body_x,
                body_start,
                fragmented: index > 0,
                digit_width: width / if index == 0 { digits as f64 } else { 1.0 },
            });
        }
    }
    None
}

fn begins_lettered_list(label: &Label, after: &[OcrLine]) -> bool {
    let Some((first, rest)) = after.split_first() else {
        return false;
    };
    let is_lettered = |line: &OcrLine, letter: &str| {
        if line_rotation(line).unwrap_or_default() != label.rotation {
            return false;
        }
        let [marker, body, ..] = line.words.as_slice() else {
            return false;
        };
        let (marker_x, marker_y, _, _) = word_box(marker, label.rotation);
        let (body_x, _, _, _) = word_box(body, label.rotation);
        marker.text == letter
            && line.reading_text().split_whitespace().next() == Some(letter)
            && marker_x > label.x
            && marker_y > label.y
            && (body_x - label.body_x).abs() <= label.digit_width
    };
    is_lettered(first, "a.") && rest.iter().take(6).any(|line| is_lettered(line, "b."))
}

fn word_box(word: &OcrWord, rotation: u16) -> (f64, f64, f64, f64) {
    reading_box((word.x, word.y, word.width, word.height), rotation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(y: f64, words: &[(&str, f64, f64, f64, f64)]) -> OcrLine {
        let mut text = String::new();
        let words = words
            .iter()
            .map(|&(word, x, vertical_offset, width, height)| {
                if !text.is_empty() {
                    text.push(' ');
                }
                let start = text.encode_utf16().count();
                text.push_str(word);
                OcrWord {
                    text: word.into(),
                    x,
                    y: y + vertical_offset,
                    width,
                    height,
                    raw_start_utf16: Some(start),
                    source_span: None,
                }
            })
            .collect();
        OcrLine {
            text,
            words,
            page: 0,
            x: 0.1,
            y,
            width: 0.4,
            height: 0.025,
            confidence: None,
            rotation: Some(0),
            correction: None,
            block_start: false,
            source_span: None,
        }
    }

    #[test]
    fn joins_split_digits_only_in_a_corroborated_exercise_sequence() {
        let item = |y, marker| {
            row(
                y,
                &[
                    (marker, 0.146, 0.0, 0.012, 0.010),
                    ("Analysez.", 0.163, 0.0, 0.068, 0.011),
                ],
            )
        };
        let mut lines = vec![
            row(
                0.10,
                &[
                    ("1", 0.114, 0.0, 0.011, 0.015),
                    ("1", 0.129, 0.0, 0.011, 0.015),
                    ("+", 0.144, 0.011, 0.005, 0.004),
                    ("Préparez.", 0.163, 0.006, 0.068, 0.011),
                ],
            ),
            item(0.125, "a."),
            item(0.180, "b."),
            row(
                0.70,
                &[
                    ("1", 0.114, 0.0, 0.011, 0.015),
                    ("2.", 0.129, 0.0, 0.023, 0.015),
                    ("Choisissez.", 0.163, 0.006, 0.075, 0.011),
                ],
            ),
            item(0.725, "a."),
            item(0.780, "b."),
        ];
        assert!(!repair(&mut lines[..3]));
        let plus = lines[0].words[2].clone();
        lines[0].words[2].height = 0.012;
        assert!(!repair(&mut lines));
        lines[0].words[2] = plus;
        assert!(repair(&mut lines));
        assert_eq!(lines[0].reading_text(), "11. Préparez.");
        assert_eq!(lines[3].reading_text(), "12. Choisissez.");
        assert_eq!(lines[0].text, "1 1 + Préparez.");
        assert!(!repair(&mut lines));
    }
}
