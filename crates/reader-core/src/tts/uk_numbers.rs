use std::borrow::Cow;

const SMALL: [&str; 20] = [
    "нуль",
    "один",
    "два",
    "три",
    "чотири",
    "п'ять",
    "шість",
    "сім",
    "вісім",
    "дев'ять",
    "десять",
    "одинадцять",
    "дванадцять",
    "тринадцять",
    "чотирнадцять",
    "п'ятнадцять",
    "шістнадцять",
    "сімнадцять",
    "вісімнадцять",
    "дев'ятнадцять",
];
const TENS: [&str; 10] = [
    "",
    "",
    "двадцять",
    "тридцять",
    "сорок",
    "п'ятдесят",
    "шістдесят",
    "сімдесят",
    "вісімдесят",
    "дев'яносто",
];
const HUNDREDS: [&str; 10] = [
    "",
    "сто",
    "двісті",
    "триста",
    "чотириста",
    "п'ятсот",
    "шістсот",
    "сімсот",
    "вісімсот",
    "дев'ятсот",
];

pub(super) fn expand(text: &str) -> Cow<'_, str> {
    let mut characters = text.char_indices().peekable();
    let mut expanded = String::new();
    let mut copied = 0;
    while let Some((start, first)) = characters.next() {
        if !first.is_ascii_digit() {
            continue;
        }
        while characters
            .peek()
            .is_some_and(|(_, next)| next.is_ascii_digit())
        {
            characters.next();
        }
        let end = characters.peek().map_or(text.len(), |&(offset, _)| offset);
        let digits = &text[start..end];
        let following = &text[end..];
        let feminine = feminine_count_target(following);
        if (digits.len() < 2 && !(feminine && digits == "2"))
            || digits.len() > 9
            || digits.starts_with('0')
            || joined_number(text[..start].chars().rev(), digits.len() == 3, 1)
            || joined_number(text[end..].chars(), true, 3)
        {
            continue;
        }
        let Ok(number) = digits.parse::<usize>() else {
            continue;
        };
        expanded.push_str(&text[copied..start]);
        expanded.push_str(&cardinal_with_gender(number, feminine));
        copied = end;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    expanded.push_str(&text[copied..]);
    Cow::Owned(expanded)
}

fn feminine_count_target(following: &str) -> bool {
    if !following.starts_with(char::is_whitespace) {
        return false;
    }
    let word = following.trim_start()
        .split(|ch: char| !ch.is_alphabetic() && ch != super::STRESS_MARK)
        .next().unwrap_or("");
    matches!(word.replace(super::STRESS_MARK, "").to_lowercase().as_str(), "руки" | "ноги")
}

fn joined_number(mut neighbour: impl Iterator<Item = char>, allow_grouping: bool, group_width: usize) -> bool {
    match neighbour.next() {
        Some(ch) if ch.is_alphabetic() || matches!(ch, '-' | '‐' | '‑' | '–') => true,
        Some('.' | ',' | ':' | '/') => neighbour.next().is_some_and(|ch| ch.is_ascii_digit()),
        Some(' ' | '\u{a0}' | '\u{2009}' | '\u{202f}') if allow_grouping => {
            // A space can be a thousands separator. Check for the adjoining digit group.
            let count = neighbour.take_while(char::is_ascii_digit).take(group_width + 1).count();
            if group_width == 1 { count > 0 } else { count == group_width }
        }
        _ => false,
    }
}

pub(crate) fn cardinal(number: usize) -> String {
    cardinal_with_gender(number, false)
}

fn cardinal_with_gender(number: usize, feminine: bool) -> String {
    let mut words = Vec::new();
    let millions = number / 1_000_000;
    if millions > 0 {
        group(&mut words, millions, false);
        words.push(counted(millions, ["мільйон", "мільйони", "мільйонів"]));
    }
    let thousands = number / 1000 % 1000;
    if thousands > 0 {
        group(&mut words, thousands, true);
        words.push(counted(thousands, ["тисяча", "тисячі", "тисяч"]));
    }
    group(&mut words, number % 1000, feminine);
    words.join(" ")
}

fn group(words: &mut Vec<&'static str>, number: usize, feminine: bool) {
    if number >= 100 {
        words.push(HUNDREDS[number / 100]);
    }
    let tail = number % 100;
    if tail >= 20 {
        words.push(TENS[tail / 10]);
    }
    let units = if tail >= 20 { tail % 10 } else { tail };
    match (units, feminine) {
        (0, _) => {}
        (1, true) => words.push("одна"),
        (2, true) => words.push("дві"),
        (units, _) => words.push(SMALL[units]),
    }
}

fn counted(number: usize, forms: [&'static str; 3]) -> &'static str {
    match (number % 100, number % 10) {
        (11..=14, _) => forms[2],
        (_, 1) => forms[0],
        (_, 2..=4) => forms[1],
        _ => forms[2],
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_standalone_cardinals_without_changing_numeric_notation() {
        assert_eq!(expand("Є 19 вікон. На сторінці 24."), "Є дев'ятнадцять вікон. На сторінці двадцять чотири.");
        let structured = "12:35, 12,50, 2024/11/24, 20 000, 12 450, 19-й, B12, 19A, 19–24.";
        assert_eq!(expand(structured), structured);
    }
}
