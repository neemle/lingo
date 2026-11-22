//! A bounded retry for the observed English clock → Ukrainian year mistranslation.

pub(super) struct ClockRetry {
    pub input: String,
    hour: u8,
    minute: u8,
}

pub(super) fn retry_input(source: &str, translated: &str) -> Option<ClockRetry> {
    let bytes = source.as_bytes();
    let mut clocks = Vec::new();
    for start in 0..bytes.len().saturating_sub(6) {
        let value = &bytes[start..];
        if !value[..3].eq_ignore_ascii_case(b"at ")
            || source[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        let colon = if value[4] == b':' {
            4
        } else if value.len() >= 8 && value[5] == b':' {
            5
        } else {
            continue;
        };
        if !value[3..colon].iter().all(u8::is_ascii_digit)
            || !value[colon + 1..colon + 3].iter().all(u8::is_ascii_digit)
        {
            continue;
        }
        let hour = value[3..colon]
            .iter()
            .fold(0, |n, digit| n * 10 + digit - b'0');
        let minute = (value[colon + 1] - b'0') * 10 + value[colon + 2] - b'0';
        let end = start + colon + 3;
        let tail = &source[end..];
        if hour > 23 || minute > 59 || !simple_tail(tail) {
            continue;
        }
        let next = tail.trim_start().to_ascii_lowercase();
        if ["a.m", "p.m", "am", "pm", "a.", "p."]
            .iter()
            .any(|prefix| next.starts_with(prefix))
        {
            return None;
        }
        clocks.push((start + 3, end, hour, minute));
    }
    let [(start, end, hour, minute)] = clocks.as_slice() else {
        return None;
    };
    if !(13..=23).contains(hour) {
        return None;
    }
    let year = format!("{hour:02}{minute:02}");
    if source.contains(&year) || !as_year(translated, &year) {
        return None;
    }
    let tail = source[*end..].strip_prefix('.').unwrap_or(&source[*end..]);
    Some(ClockRetry {
        input: format!("{}{}:{minute:02} p.m.{tail}", &source[..*start], hour - 12),
        hour: *hour,
        minute: *minute,
    })
}

fn simple_tail(tail: &str) -> bool {
    match tail.chars().next() {
        None => true,
        Some(c) if c.is_whitespace() => true,
        Some('.' | ',') => tail[1..].chars().next().is_none_or(char::is_whitespace),
        _ => false,
    }
}

fn as_year(text: &str, year: &str) -> bool {
    let words: Vec<_> = text.split_whitespace().map(trim).collect();
    words
        .windows(2)
        .any(|pair| pair[0] == year && matches!(pair[1], "року" | "році"))
}

fn trim(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric())
}

impl ClockRetry {
    /// Require affirmative clock evidence before replacing the original hypothesis.
    pub fn accepts(&self, text: &str) -> bool {
        if as_year(text, &format!("{:02}{:02}", self.hour, self.minute)) {
            return false;
        }
        let bytes = text.as_bytes();
        for start in 0..bytes.len() {
            if !bytes[start].is_ascii_digit() || start > 0 && bytes[start - 1].is_ascii_digit() {
                continue;
            }
            let mut separator = start;
            while separator < bytes.len() && bytes[separator].is_ascii_digit() {
                separator += 1;
            }
            if separator - start > 2 || !matches!(bytes.get(separator), Some(b':' | b'.')) {
                continue;
            }
            let Ok(hour) = text[start..separator].parse::<u8>() else {
                continue;
            };
            let mut minute_start = separator + 1;
            while bytes.get(minute_start) == Some(&b' ') {
                minute_start += 1;
            }
            let end = minute_start + 2;
            if end > bytes.len()
                || !bytes[minute_start..end].iter().all(u8::is_ascii_digit)
                || bytes.get(end).is_some_and(u8::is_ascii_digit)
            {
                continue;
            }
            let Ok(minute) = text[minute_start..end].parse::<u8>() else {
                continue;
            };
            if minute != self.minute {
                continue;
            }
            let after = text[end..].split_whitespace().take(2).map(trim);
            if after.clone().any(|word| matches!(word, "року" | "році")) {
                continue;
            }
            if hour == self.hour {
                return true;
            }
            if hour != self.hour - 12 {
                continue;
            }
            let before = text[..start].split_whitespace().rev().take(2).map(trim);
            if before.chain(after).any(|word| match self.hour {
                13..=17 => matches!(word, "дня" | "вдень" | "удень" | "пополудні"),
                _ => matches!(word, "вечора" | "ввечері" | "увечері"),
            }) {
                return true;
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_only_one_explicit_clock_collapsed_into_a_year() {
        let source = "Flight AF123 leaves on November 22 at 18:45.";
        let retry =
            retry_input(source, "Рейс AF123 відправляється 22 листопада 1845 року.").unwrap();
        assert_eq!(
            retry.input,
            "Flight AF123 leaves on November 22 at 6:45 p.m."
        );
        assert!(retry.accepts("Рейс AF123 відправляється о 6: 45 вечора."));
        assert!(retry.accepts("Рейс AF123 відправляється о 18.45."));
        assert!(!retry.accepts("Рейс AF123 відправляється о 6:45."));
        assert!(!retry.accepts("Рейс AF123 відправляється о 18:40."));
        assert!(!retry.accepts("Рейс AF123 відправляється 1845 року."));
        assert!(retry_input(source, "Рейс AF123 відправляється о 18:45.").is_none());
    }
    #[test]
    fn leaves_ambiguous_or_unsupported_source_forms_untouched() {
        for source in [
            "at 00:15",
            "at 6:45",
            "at 12:45",
            "at 24:45",
            "at 18:60",
            "at 18:45 p.m.",
            "at 18:45:30",
            "at 18:45-19:45",
            "format 18:45",
            "at 18:45 and at 19:30",
            "at 18:45 and at 06:30",
            "at 18:45 and at 6:30",
            "In 1845, at 18:45",
        ] {
            assert!(retry_input(source, "1845 року").is_none(), "{source}");
        }
    }
}
