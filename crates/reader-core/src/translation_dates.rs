use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const FRENCH_MONTHS: [&str; 12] = [
    "janvier",
    "février",
    "mars",
    "avril",
    "mai",
    "juin",
    "juillet",
    "août",
    "septembre",
    "octobre",
    "novembre",
    "décembre",
];
const UKRAINIAN_MONTH_PHRASES: [&str; 12] = [
    "у січні", "у лютому", "у березні", "у квітні", "у травні", "у червні",
    "у липні", "у серпні", "у вересні", "у жовтні", "у листопаді", "у грудні",
];

pub(crate) fn ukrainian_month_phrase(french_month: &str) -> Option<&'static str> {
    FRENCH_MONTHS.iter().position(|&month| month == french_month)
        .map(|index| UKRAINIAN_MONTH_PHRASES[index])
}
const UKRAINIAN_MONTHS: [&str; 12] = [
    "січня",
    "лютого",
    "березня",
    "квітня",
    "травня",
    "червня",
    "липня",
    "серпня",
    "вересня",
    "жовтня",
    "листопада",
    "грудня",
];
const UKRAINIAN_ORDINALS: [&str; 20] = [
    "першого",
    "другого",
    "третього",
    "четвертого",
    "п'ятого",
    "шостого",
    "сьомого",
    "восьмого",
    "дев'ятого",
    "десятого",
    "одинадцятого",
    "дванадцятого",
    "тринадцятого",
    "чотирнадцятого",
    "п'ятнадцятого",
    "шістнадцятого",
    "сімнадцятого",
    "вісімнадцятого",
    "дев'ятнадцятого",
    "двадцятого",
];

#[derive(Clone, Copy, PartialEq, Eq)]
struct Date {
    day: u8,
    month: usize,
    year: u16,
}

impl Date {
    fn matching_fields(self, other: Self) -> u8 {
        u8::from(self.day == other.day)
            + u8::from(self.month == other.month)
            + u8::from(self.year == other.year)
    }
}

struct LocatedDate {
    date: Date,
    span: Range<usize>,
    uppercase: bool,
}

pub(crate) fn contains_french_date(text: &str) -> bool {
    !dates(text, false).is_empty()
}

pub(crate) fn preserve_french_dates(source: &str, target: &mut String) {
    let source = dates(source, false);
    if source.is_empty() {
        return;
    }
    let mut reserved = vec![false; source.len()];
    let rendered = dates(target, true);
    for date in &rendered {
        if let Some(index) = source
            .iter()
            .enumerate()
            .position(|(index, entry)| !reserved[index] && entry.date == date.date)
        {
            reserved[index] = true;
        }
    }
    for date in rendered.into_iter().rev() {
        if source.iter().any(|entry| entry.date == date.date) {
            continue;
        }
        let mut matches = source.iter().enumerate().filter(|(index, entry)| {
            !reserved[*index] && entry.date.matching_fields(date.date) >= 2
        });
        let Some((index, original)) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        reserved[index] = true;
        let month = UKRAINIAN_MONTHS[original.date.month];
        let month = if date.uppercase {
            month.to_uppercase()
        } else {
            month.to_owned()
        };
        target.replace_range(
            date.span,
            &format!("{} {month} {}", original.date.day, original.date.year),
        );
    }
}

fn dates(text: &str, ukrainian: bool) -> Vec<LocatedDate> {
    if !text.bytes().any(|byte| byte.is_ascii_digit()) {
        return Vec::new();
    }
    let months = if ukrainian {
        &UKRAINIAN_MONTHS
    } else {
        &FRENCH_MONTHS
    };
    let words: Vec<_> = text.unicode_word_indices().collect();
    words
        .windows(3)
        .enumerate()
        .filter_map(|(index, words3)| {
            let [(mut start, word), (month_start, name), (year_start, year)] = *words3 else {
                return None;
            };
            let number_year = year
                .parse()
                .ok()
                .filter(|year| (1000..=9999).contains(year))?;
            if !text[start + word.len()..month_start]
                .chars()
                .all(char::is_whitespace)
                || !text[month_start + name.len()..year_start]
                    .chars()
                    .all(char::is_whitespace)
            {
                return None;
            }
            let lowercase = name.to_lowercase();
            let month = months.iter().position(|&month| month == lowercase)?;
            let day = if ukrainian {
                if let Ok(number) = word.parse::<u8>() {
                    number
                } else {
                    let mut day = ukrainian_ordinal(word)?;
                    if let Some(&(previous_start, previous)) =
                        index.checked_sub(1).and_then(|i| words.get(i))
                    {
                        if text[previous_start + previous.len()..start]
                            .chars()
                            .all(char::is_whitespace)
                        {
                            match (previous.to_lowercase().as_str(), day) {
                                ("двадцять", 1..=9) => {
                                    start = previous_start;
                                    day += 20;
                                }
                                ("тридцять", 1) => {
                                    start = previous_start;
                                    day = 31;
                                }
                                _ => {}
                            }
                        }
                    }
                    day
                }
            } else if word.eq_ignore_ascii_case("1er") {
                1
            } else {
                word.parse().ok()?
            };
            (1..=31).contains(&day).then_some(LocatedDate {
                date: Date {
                    day,
                    month,
                    year: number_year,
                },
                span: start..year_start + year.len(),
                uppercase: name.chars().all(char::is_uppercase),
            })
        })
        .collect()
}

fn ukrainian_ordinal(word: &str) -> Option<u8> {
    let word = word.to_lowercase().replace(['’', 'ʼ'], "'");
    if word == "тридцятого" {
        return Some(30);
    }
    UKRAINIAN_ORDINALS
        .iter()
        .position(|&day| day == word)
        .map(|index| index as u8 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_the_reported_october_date_with_a_written_ordinal() {
        let source = "LE 25 OCTOBRE 1892, la note parut dans Le Temps.";
        let mut translated = "ГЛАВА І П’ЯТОГО ЖОВТНЯ 1892 року з'явилася нотатка.".to_owned();
        preserve_french_dates(source, &mut translated);
        assert_eq!(translated, "ГЛАВА І 25 ЖОВТНЯ 1892 року з'явилася нотатка.");
    }

    #[test]
    fn preserves_correct_reordered_dates_and_ignores_ambiguous_matches() {
        let source = "Le 15 février 1815, puis le 15 avril 1815.";
        let correct = "15 квітня 1815 після подій 15 лютого 1815";
        let mut translated = correct.to_owned();
        preserve_french_dates(source, &mut translated);
        assert_eq!(translated, correct);

        let uncertain = "15 березня 1815";
        let mut translated = uncertain.to_owned();
        preserve_french_dates(source, &mut translated);
        assert_eq!(translated, uncertain);
    }

    #[test]
    fn restores_the_year_without_leaving_part_of_a_written_day_behind() {
        let source = "Le 25 octobre 1892.";
        let mut translated = "Двадцять п’ятого жовтня 1893 року.".to_owned();
        preserve_french_dates(source, &mut translated);
        assert_eq!(translated, "25 жовтня 1892 року.");
    }
}
