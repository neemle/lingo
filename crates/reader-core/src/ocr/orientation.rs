use super::OcrLine;
use crate::Result;

/// Fold trustworthy sideways or inverted lines into the portable result.
/// Both recognizers report coordinates in the original preview.
pub fn merge_rotated_lines(
    mut primary: Vec<OcrLine>,
    native: Vec<OcrLine>,
) -> Result<Vec<OcrLine>> {
    if primary.len() > 8192 || native.len() > 8192 {
        return Err("Завеликий набір результатів OCR.".into());
    }
    let mut word_count: usize = primary.iter().map(|line| line.words.len()).sum();
    for mut candidate in native.into_iter().filter(|line| {
        line.confidence.is_some_and(|score| score >= 0.85)
            && line.text.chars().filter(|letter| letter.is_alphabetic()).take(3).count() == 3
    }).take(256) {
        let mut overlaps = primary.iter().enumerate().filter(|(_, line)| same_area(line, &candidate));
        let first = overlaps.next().map_or(primary.len(), |(index, _)| index);
        if primary.get(first).is_some_and(|line| line.text.trim() == candidate.text.trim())
            && overlaps.next().is_none()
        {
            continue;
        }
        let removed_words = primary.iter().filter(|line| same_area(line, &candidate))
            .map(|line| line.words.len()).sum::<usize>();
        let new_count = word_count - removed_words + candidate.words.len();
        if new_count > 65_536 || (first == primary.len() && primary.len() >= 8192) {
            continue;
        }
        // Keep a rotated line separate from adjacent upright prose.
        candidate.block_start = true;
        primary.retain(|line| !same_area(line, &candidate));
        primary.insert(first, candidate);
        word_count = new_count;
    }
    Ok(primary)
}

fn same_area(left: &OcrLine, right: &OcrLine) -> bool {
    let width = (left.x + left.width).min(right.x + right.width) - left.x.max(right.x);
    let height = (left.y + left.height).min(right.y + right.height) - left.y.max(right.y);
    width > 0.0 && height > 0.0 && width * height >= 0.55
        * (left.width * left.height).min(right.width * right.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ocr::OcrWord;

    fn row(text: &str, first: (f64, f64), last: (f64, f64)) -> OcrLine {
        let words = [first, last].into_iter().zip(text.split_whitespace())
            .map(|((x, y), word)| OcrWord {
                text: word.into(), x, y, width: 0.02, height: 0.02,
                raw_start_utf16: None, source_span: None,
            }).collect();
        OcrLine {
            page: 0, text: text.into(), x: first.0.min(last.0), y: first.1.min(last.1),
            width: (first.0 - last.0).abs() + 0.02, height: (first.1 - last.1).abs() + 0.02,
            confidence: Some(0.95), correction: None, block_start: false, words, source_span: None,
        }
    }

    #[test]
    fn keeps_upright_prose_and_anchors_for_three_rotated_directions() {
        let upright = row("Le bateau", (0.1, 0.1), (0.3, 0.1));
        let sideways = row("La bibliothèque", (0.8, 0.1), (0.8, 0.4));
        let inverted = row("Le théâtre", (0.6, 0.7), (0.4, 0.7));
        let reverse_sideways = row("Notre jardin", (0.1, 0.8), (0.1, 0.5));
        let mut garbled = inverted.clone();
        garbled.text = "as tneIht".into();
        let merged = merge_rotated_lines(vec![upright.clone(), garbled, sideways.clone()],
            vec![sideways, inverted, reverse_sideways]).unwrap();
        assert_eq!(merged.iter().map(|line| line.text.as_str()).collect::<Vec<_>>(),
            ["Le bateau", "Le théâtre", "La bibliothèque", "Notre jardin"]);
        assert_eq!((merged[1].words[0].x, merged[3].words[0].y), (0.6, 0.8));
        assert!(merged[1].block_start && merged[3].block_start);
    }
}
