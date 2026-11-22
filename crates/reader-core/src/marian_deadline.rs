//! Preserve explicit Ukrainian weekday deadlines in French.

use unicode_segmentation::UnicodeSegmentation;

pub(crate) struct Deadline {
    french_day: &'static str,
    by_day: bool,
}

pub(crate) fn find(source: &str) -> Option<Deadline> {
    // Limit the repair to an explicit final weekday in a simple clause.
    let source_text = source.trim_end();
    let clause = source_text.trim_end_matches(['.', '!', '?']);
    if clause.contains([',', ':', ';', '.', '!', '?', '\n', '—']) {
        return None;
    }
    let mut words = clause.unicode_word_indices().rev();
    let (day_start, day) = words.next()?;
    let (start, marker) = words.next()?;
    if day_start + day.len() != clause.len()
        || !clause[start + marker.len()..day_start]
            .chars()
            .all(char::is_whitespace)
    {
        return None;
    }
    let by_day = match marker.to_lowercase().as_str() {
        "раніше" if words.any(|(_, word)| word.to_lowercase() == "не") => false,
        "до" if punctual_deadline(words.map(|(_, word)| word)) => true,
        _ => return None,
    };
    let french_day = match day.to_lowercase().replace(['’', 'ʼ'], "'").as_str() {
        "понеділка" => "lundi",
        "вівторка" => "mardi",
        "середи" => "mercredi",
        "четверга" => "jeudi",
        "п'ятниці" => "vendredi",
        "суботи" => "samedi",
        "неділі" => "dimanche",
        _ => return None,
    };
    Some(Deadline { french_day, by_day })
}

fn punctual_deadline<'a>(words: impl Iterator<Item = &'a str>) -> bool {
    let mut punctual = false;
    for word in words {
        match word.to_lowercase().as_str() {
            "не" | "або" | "чи" | "якщо" | "коли" | "доки" => return false,
            "надішлю" | "надішлеш" | "надішле" | "надішлемо" | "надішлете" | "надішлють"
            | "завершу" | "завершиш" | "завершить" | "завершимо" | "завершите" | "завершать" =>
            {
                punctual = true;
            }
            _ => {}
        }
    }
    punctual
}

impl Deadline {
    pub(crate) fn cache_revision(&self) -> u8 {
        if self.by_day {
            6
        } else {
            5
        }
    }

    fn bare_day_prefix<'a>(&self, clause: &'a str) -> Option<&'a str> {
        let prefix = clause.strip_suffix(self.french_day)?.strip_suffix(' ')?;
        let prefix = prefix.strip_suffix(" le").unwrap_or(prefix);
        let last = prefix.split_whitespace().next_back()?.to_lowercase();
        if last.ends_with(['’', '\''])
            || matches!(
                last.as_str(),
                "à" | "au"
                    | "après"
                    | "avant"
                    | "pour"
                    | "ce"
                    | "du"
                    | "de"
                    | "dès"
                    | "jusqu’à"
                    | "jusqu'à"
                    | "tard"
                    | "et"
                    | "ou"
            )
        {
            return None;
        }
        Some(prefix)
    }

    pub(super) fn restore_comparison(&self, translated: &str) -> Option<String> {
        let clause = translated
            .trim_end_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '!' | '?'));
        if self.by_day {
            let prefix = self.bare_day_prefix(clause)?;
            return Some(format!(
                "{prefix} d’ici {}{}",
                self.french_day,
                &translated[clause.len()..]
            ));
        }
        let malformed = format!("pas plus tôt le {}", self.french_day);
        let prefix = clause.strip_suffix(&malformed)?;
        if prefix
            .chars()
            .next_back()
            .is_some_and(|c| !c.is_whitespace())
        {
            return None;
        }
        Some(format!(
            "{prefix}pas plus tôt que {}{}",
            self.french_day,
            &translated[clause.len()..]
        ))
    }

    pub(super) fn accepts(&self, candidate: &str) -> bool {
        let lower = candidate.to_lowercase();
        let words: Vec<_> = lower.unicode_words().collect();
        if self.by_day {
            return words.windows(2).any(|window| {
                matches!(window[0], "avant" | "pour" | "d’ici" | "d'ici")
                    && window[1] == self.french_day
            }) || words
                .windows(4)
                .any(|window| window == ["au", "plus", "tard", self.french_day]);
        }
        words
            .windows(3)
            .any(|window| window == ["pas", "avant", self.french_day])
            || words
                .windows(5)
                .any(|window| window == ["pas", "plus", "tôt", "que", self.french_day])
    }

    pub(super) fn preserves_clause(&self, original: &str, candidate: &str) -> bool {
        if self.by_day || !self.accepts(candidate) {
            return false;
        }
        let without_deadline = |text: &str| {
            let mut words: Vec<_> = text
                .to_lowercase()
                .unicode_words()
                .map(str::to_owned)
                .collect();
            if words.last().is_some_and(|last| last == self.french_day) {
                words.pop();
                while words.last().is_some_and(|last| {
                    matches!(last.as_str(), "le" | "avant" | "plus" | "tôt" | "que")
                }) {
                    words.pop();
                }
            }
            words
        };
        without_deadline(original) == without_deadline(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekday_retry_requires_explicit_negative_source_comparison() {
        let source = "Вона не прокинеться раніше середи.";
        let deadline = find(source).unwrap();
        assert_eq!(
            deadline
                .restore_comparison("Elle ne se réveillera pas plus tôt le mercredi.")
                .as_deref(),
            Some("Elle ne se réveillera pas plus tôt que mercredi.")
        );
        assert!(deadline
            .restore_comparison("Elle ne se réveillera pas le mercredi.")
            .is_none());
        assert!(find("Я не приїду раніше п’ятниці.").is_some());
        assert!(find("Вона не прокинеться в середу.").is_none());
        assert!(find("Вона прокинеться раніше середи.").is_none());
        assert!(find("Вона не прокинеться до середи.").is_none());
        assert!(find("Вона не прокинеться раніше середи, а він — у четвер.").is_none());
    }

    #[test]
    fn new_comparison_cannot_change_decoded_action_or_weekday() {
        let deadline = find("Вона не прокинеться раніше середи.").unwrap();
        let baseline = "Elle ne se réveillera pas plus tôt le mercredi.";
        assert!(!deadline.accepts(baseline));
        assert!(deadline.preserves_clause(baseline, "Elle ne se réveillera pas avant mercredi."));
        assert!(
            deadline.preserves_clause(baseline, "Elle ne se réveillera pas plus tôt que mercredi.")
        );
        assert!(!deadline.preserves_clause(baseline, "Elle ne se réveillera pas avant jeudi."));
        assert!(!deadline.preserves_clause(baseline, "Elle ne dormira pas avant mercredi."));
        assert!(!deadline.preserves_clause(baseline, "Elle se réveillera avant mercredi."));
    }
}
