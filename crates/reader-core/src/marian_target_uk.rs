use unicode_segmentation::UnicodeSegmentation;
use whatlang::Lang;

pub(super) fn clearly_russian(text: &str) -> bool {
    distinguish(text) == Some(Lang::Rus)
}

pub(super) fn clearly_ukrainian(text: &str) -> bool {
    distinguish(text) == Some(Lang::Ukr)
}

fn distinguish(text: &str) -> Option<Lang> {
    let sample = outside_quotes(text);
    if sample
        .unicode_words()
        .filter(|word| {
            word.chars()
                .any(|c| ('\u{0400}'..='\u{04ff}').contains(&c) && c.is_alphabetic())
        })
        .take(3)
        .count()
        < 3
    {
        return None;
    }

    let russian = sample.contains(['ы', 'э', 'ё', 'ъ', 'Ы', 'Э', 'Ё', 'Ъ']);
    let ukrainian = sample.contains(['і', 'ї', 'є', 'ґ', 'І', 'Ї', 'Є', 'Ґ']);
    let expected = match (russian, ukrainian) {
        (true, false) => Lang::Rus,
        (false, true) => Lang::Ukr,
        _ => return None,
    };
    let detected = whatlang::detect(&sample)?;
    // Whatlang's generic reliability threshold misses short but distinctive sentences.
    (detected.lang() == expected && detected.confidence() >= 0.45).then_some(expected)
}

fn outside_quotes(text: &str) -> String {
    let mut sample = String::with_capacity(text.len());
    let mut closer = None;
    for ch in text.chars() {
        if let Some(end) = closer {
            if ch == end {
                closer = None;
            }
            continue;
        }
        closer = match ch {
            '«' => Some('»'),
            '„' => Some('“'),
            '“' => Some('”'),
            '"' => Some('"'),
            _ => None,
        };
        sample.push(if closer.is_some() { ' ' } else { ch });
    }
    sample
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_observed_wrong_language_from_ukrainian_candidate() {
        let wrong = "Мы не уедем до понедельника.";
        let right = "Ми не підемо до понеділка.";
        assert!(clearly_russian(wrong));
        assert!(!clearly_ukrainian(wrong));
        assert!(clearly_ukrainian(right));
        assert!(!clearly_russian(right));
    }

    #[test]
    fn abstains_on_short_ambiguous_and_mixed_evidence() {
        for text in [
            "Мы",
            "Москва",
            "Станція Метро",
            "Мэр Ёлкин прибув сьогодні.",
        ] {
            assert!(!clearly_russian(text), "{text}");
            assert!(!clearly_ukrainian(text), "{text}");
        }
        assert!(!clearly_russian(
            "Він написав: «Мы не уедем до понедельника»."
        ));
        assert!(clearly_ukrainian(
            "Він сьогодні написав у листі: «Мы не уедем до понедельника»."
        ));
    }
}
