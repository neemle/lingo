use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

const SMALL: [&str; 17] = [
    "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix",
    "onze", "douze", "treize", "quatorze", "quinze", "seize",
];
const TENS: [&str; 5] = ["vingt", "trente", "quarante", "cinquante", "soixante"];
const HYPHENS: [char; 3] = ['-', '‐', '‑'];
const MAX_WORDS: usize = 13;
pub(crate) const NORMALIZATION_REVISION: &str = "fr-uk-cardinals-v3";

pub(crate) fn spoken_cardinal(number: usize) -> Option<String> {
    if number > 999_999 {
        return None;
    }
    let mut words = Vec::new();
    let thousands = number / 1000;
    if thousands > 0 {
        if thousands > 1 {
            spell_group(thousands, false, &mut words);
        }
        words.push("mille");
    }
    let remainder = number % 1000;
    if remainder > 0 || number == 0 {
        spell_group(remainder, true, &mut words);
    }
    Some(words.join(" "))
}

fn spell_group(number: usize, final_group: bool, words: &mut Vec<&'static str>) {
    let hundreds = number / 100;
    let tail = number % 100;
    if hundreds > 0 {
        if hundreds > 1 {
            words.push(SMALL[hundreds]);
        }
        words.push(if hundreds > 1 && tail == 0 && final_group { "cents" } else { "cent" });
        if tail == 0 {
            return;
        }
    }
    let units = if tail >= 80 {
        words.push(if tail == 80 && final_group { "quatre-vingts" } else { "quatre-vingt" });
        tail - 80
    } else if tail >= 20 {
        let tens = (tail / 10).min(6);
        words.push(TENS[tens - 2]);
        let units = tail - tens * 10;
        if matches!(units, 1 | 11) {
            words.push("et");
        }
        units
    } else {
        if tail == 0 {
            words.push(SMALL[0]);
        }
        tail
    };
    match units {
        0 => {}
        1..=16 => words.push(SMALL[units]),
        17..=19 => words.push(["dix-sept", "dix-huit", "dix-neuf"][units - 17]),
        _ => unreachable!("French cardinal remainder below twenty"),
    }
}

fn small(word: &str) -> Option<usize> {
    if word == "une" { Some(1) } else { SMALL.iter().position(|&value| value == word) }
}

fn regional_tens(word: &str) -> Option<usize> {
    match word {
        "septante" => Some(70),
        "huitante" | "octante" => Some(80),
        "nonante" => Some(90),
        _ => None,
    }
}

fn under_hundred(words: &[&str]) -> Option<usize> {
    if let Some(base) = words.first().and_then(|word| regional_tens(word)) {
        let unit = match &words[1..] {
            [] => 0,
            ["et", "un" | "une"] => 1,
            [word] => small(word).filter(|value| (1..=9).contains(value))?,
            _ => return None,
        };
        return Some(base + unit);
    }
    let (base, rest) = match words {
        [word] => return small(word).or_else(|| TENS.iter().position(|&value| value == *word).map(|i| 20 + i * 10)),
        ["dix", word] => return small(word).filter(|unit| (7..=9).contains(unit)).map(|unit| 10 + unit),
        ["quatre", "vingt" | "vingts"] => return Some(80),
        ["quatre", "vingt", rest @ ..] if !rest.is_empty() => (80, rest),
        [tens, rest @ ..] if !rest.is_empty() =>
            (20 + 10 * TENS.iter().position(|&word| word == *tens)?, rest),
        _ => return None,
    };
    let unit = match (base, rest) {
        (20..=60, ["et", "un" | "une"]) => 1,
        (60, ["et", "onze"]) => 11,
        (60 | 80, ["dix", word]) => 10 + small(word).filter(|value| (7..=9).contains(value))?,
        (60 | 80, [word]) => small(word).filter(|value| (1..=16).contains(value)
            && (base == 80 || !matches!(*value, 1 | 11)))?,
        (_, [word]) => small(word).filter(|value| (2..=9).contains(value))?,
        _ => return None,
    };
    Some(base + unit)
}

fn under_thousand(words: &[&str]) -> Option<usize> {
    let (hundreds, rest) = match words {
        ["cent", rest @ ..] => (1, rest),
        [word, "cent", rest @ ..] => (small(word).filter(|value| (2..=9).contains(value))?, rest),
        [word, "cents"] => return Some(100 * small(word).filter(|value| (2..=9).contains(value))?),
        _ => return under_hundred(words),
    };
    Some(hundreds * 100 + if rest.is_empty() { 0 } else { under_hundred(rest)? })
}

fn parse(words: &[&str]) -> Option<usize> {
    if let Some(index) = words.iter().position(|&word| word == "mille") {
        let thousands = if index == 0 { 1 } else { under_thousand(&words[..index]).filter(|value| *value >= 2)? };
        let rest = &words[index + 1..];
        let units = if rest.is_empty() { 0 } else { under_thousand(rest)? };
        return (thousands > 0).then_some(thousands * 1000 + units);
    }
    under_thousand(words)
}

pub(crate) fn compound(text: &str) -> Option<usize> {
    let normalized: String = text.trim().to_lowercase().nfc().collect();
    let mut words = Vec::new();
    let mut end = 0;
    for (start, word) in normalized.split_word_bound_indices().filter(|(_, part)| part.starts_with(char::is_alphabetic)) {
        if words.len() == MAX_WORDS || !joins_number(&normalized[end..start]) || words.is_empty() && start != 0 {
            return None;
        }
        words.push(word);
        end = start + word.len();
    }
    let complete = end == normalized.len();
    match words.as_slice() {
        [word] if complete => regional_tens(word),
        [_, _, ..] if complete => parse(&words),
        _ => None,
    }
}

fn joins_number(gap: &str) -> bool {
    let gap = gap.trim();
    gap.is_empty() || matches!(gap, "-" | "‐" | "‑")
}

fn number_start(word: &str) -> bool {
    small(word).is_some() || TENS.contains(&word) || regional_tens(word).is_some()
        || matches!(word, "cent" | "mille")
}

pub(crate) fn digits_in_text(text: &str) -> String {
    let spans: Vec<_> = text.split_word_bound_indices()
        .filter(|(_, part)| part.starts_with(char::is_alphabetic)).collect();
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    let mut index = 0;
    while let Some(&(start, word)) = spans.get(index) {
        let first: String = word.to_lowercase().nfc().collect();
        if !number_start(&first) {
            index += 1;
            continue;
        }
        // A hyphen may connect a cardinal-looking fragment to a larger word.
        if text[..start].ends_with(HYPHENS) {
            index += 1;
            continue;
        }
        let mut end = start + word.len();
        let mut best = regional_tens(&first)
            .filter(|_| !text[end..].starts_with(HYPHENS))
            .map(|value| (end, 1, value));
        let mut words = vec![first];
        for &(next, word) in spans.iter().skip(index + 1).take(MAX_WORDS - 1) {
            if !joins_number(&text[end..next]) { break; }
            let word_lower: String = word.to_lowercase().nfc().collect();
            if !number_start(&word_lower) && !matches!(word_lower.as_str(), "et" | "vingts" | "cents") { break; }
            words.push(word_lower);
            end = next + word.len();
            if !text[end..].starts_with(HYPHENS) {
                let parts: Vec<_> = words.iter().map(String::as_str).collect();
                if let Some(value) = parse(&parts) {
                    best = Some((end, words.len(), value));
                }
            }
        }
        if let Some((end, count, number)) = best {
            output.push_str(&text[copied..start]);
            output.push_str(&number.to_string());
            copied = end;
            index += count;
        } else {
            index += 1;
        }
    }
    output.push_str(&text[copied..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaced_and_hyphenated_numbers_including_years() {
        for (text, value) in [
            ("dix-neuf", 19), ("dix neuf", 19), ("vingt et un", 21),
            ("soixante et onze", 71), ("quatre‑vingt dix neuf", 99),
            ("mille huit cent quatre-vingt-douze", 1892), ("deux cent un", 201),
            ("quatre-vingt-un mille deux cents", 81_200),
            ("septante et un", 71), ("huitante-trois", 83), ("octante", 80),
            ("deux cent nonante-neuf", 299), ("mille septante et une", 1071),
        ] {
            assert_eq!(compound(text), Some(value), "{text}");
        }
        assert_eq!(digits_in_text("Il a dix neuf ans. Le 25 octobre mille huit cent quatre-vingt-douze."),
            "Il a 19 ans. Le 25 octobre 1892.");
        assert_eq!(digits_in_text("Dans les années nonante, il avait septante et un ans et comptait huitante-trois livres."),
            "Dans les années 90, il avait 71 ans et comptait 83 livres.");
    }

    #[test]
    fn leaves_standalone_ambiguous_words_and_noncardinal_hyphenated_words_intact() {
        for text in ["un bateau neuf", "le dix-neuvième jour", "avant-dix-neuf", "vingt, et un marin",
            "sa nonante-huitième année", "association-nonante"] {
            assert_eq!(digits_in_text(text), text);
        }
        assert_eq!(compound("neuf"), None);
    }

    #[test]
    fn selected_cardinals_translate_without_installing_language_packages() {
        use crate::translation::{SelectedTranslation, TranslationSelection};
        let directory = tempfile::tempdir().unwrap();
        let mut store = crate::Store::open(&directory.path().join("reader.sqlite")).unwrap();
        for (number, translation) in [
            ("dix neuf", "дев’ятнадцять"),
            ("nonante-neuf", "дев’яносто дев’ять"),
            ("huitante", "вісімдесят"),
        ] {
            let source = format!("Il a {number} ans.");
            let document = store.import("page.txt", source.as_bytes(), "fr").unwrap();
            let selected = TranslationSelection {
                page: None, revision_id: document.current_revision_id.unwrap(),
                start_utf16: "Il a ".len(), text: number.into(),
            };
            let Some(SelectedTranslation::Cardinal { text, source_text }) =
                store.translate_selected_text(&document.id, "uk", &selected).unwrap()
            else { panic!("expected deterministic translation"); };
            assert_eq!(text, translation);
            assert_eq!(source_text, number);
        }
    }
}
