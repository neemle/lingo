use unicode_segmentation::UnicodeSegmentation;
use whatlang::Lang;

const FRENCH_MANNER_OF_WALKING: &str = " de son pas souple et fort";

pub(super) fn french_manner_of_walking(text: &str) -> Option<usize> {
    text.match_indices(FRENCH_MANNER_OF_WALKING)
        .find(|(start, phrase)| {
            !text[start + phrase.len()..].starts_with(char::is_alphabetic)
        })
        .map(|(start, _)| start)
}

pub(super) fn clarify_french_manner_of_walking(text: &str) -> Option<String> {
    let start = french_manner_of_walking(text)?;
    let mut clarified = text.to_owned();
    clarified.replace_range(
        start..start + FRENCH_MANNER_OF_WALKING.len(),
        " d'une démarche souple et forte",
    );
    Some(clarified)
}

pub(super) fn clearly_russian(text: &str) -> bool {
    distinguish(text) == Some(Lang::Rus)
}

pub(super) fn clearly_ukrainian(text: &str) -> bool {
    distinguish(text) == Some(Lang::Ukr)
}

pub(super) fn repair_french_affirmative(source: &str, target: &str) -> Option<String> {
    let trim_prefix = |ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-');
    let source = source.trim_start_matches(trim_prefix).to_lowercase();
    let tail = source.strip_prefix("oui,")?.trim_start();
    let opening = target.trim_start_matches(trim_prefix);
    let suffix = opening.strip_prefix("Да,").or_else(|| opening.strip_prefix("да,"))?;
    let person = tail.strip_prefix("chez ").and_then(|rest|
        rest.strip_prefix("l’").or_else(|| rest.strip_prefix("l'"))
    ).map(|rest| rest.trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '!' | '?')));
    let addressed = suffix.trim_start();
    if let Some((preposition, noun)) = addressed.split_once(' ') {
        let noun = noun.trim_end_matches(['.', '!', '?']);
        if matches!(preposition, "в" | "у") && noun.ends_with("аторе") &&
            noun.chars().all(char::is_alphabetic) &&
            person.is_some_and(|word| word.ends_with("ateur") && word.chars().all(char::is_alphabetic))
        {
            return Some(format!("{}Так, в {}а{}", &target[..target.len() - opening.len()],
                noun.trim_end_matches('е'), &addressed[preposition.len() + 1 + noun.len()..]));
        }
    }
    let repaired = format!("{}Так,{suffix}", &target[..target.len() - opening.len()]);
    (!clearly_russian(&repaired)).then_some(repaired)
}

fn distinguish(text: &str) -> Option<Lang> {
    let sample = outside_quotes(text);
    // An independent affirmative is distinctive even when the rest is made
    // up of shared vocabulary. Keep ambiguous words and quotations untouched.
    let opening = sample.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-'));
    let mut characters = opening.chars().flat_map(char::to_lowercase);
    if characters.next() == Some('д') && characters.next() == Some('а') &&
        characters.next().is_some_and(|ch| matches!(ch, ',' | '.' | '!' | '?' | '…'))
    {
        return Some(Lang::Rus);
    }
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
    fn clarifies_french_walking_manner_without_changing_the_action() {
        let input = "Le jeune homme part de son pas souple et fort donner ses ordres aux marins.";
        let expected = "Le jeune homme part d'une démarche souple et forte donner ses ordres aux marins.";
        assert_eq!(clarify_french_manner_of_walking(input).as_deref(), Some(expected));
        assert!(clarify_french_manner_of_walking("Il parle de son pas souple et fortuit.").is_none());
    }

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
    fn rejects_russian_affirmatives_in_short_ukrainian_translations() {
        assert!(clearly_russian("Да, в узурпаторе."));
        assert!(clearly_russian("— ДА!"));
        assert!(!clearly_russian("Так, в узурпатора."));
        assert!(!clearly_russian("Давня історія."));
        assert!(!clearly_russian("«Да!» — відповів він."));
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
