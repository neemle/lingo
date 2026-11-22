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
        if digits.len() < 2
            || digits.len() > 9
            || digits.starts_with('0')
            || text[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphabetic)
            || characters
                .peek()
                .is_some_and(|(_, next)| next.is_alphabetic())
        {
            continue;
        }
        let Ok(number) = digits.parse::<usize>() else {
            continue;
        };
        expanded.push_str(&text[copied..start]);
        expanded.push_str(&cardinal(number));
        copied = end;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    expanded.push_str(&text[copied..]);
    Cow::Owned(expanded)
}

fn cardinal(number: usize) -> String {
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
    group(&mut words, number % 1000, false);
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
