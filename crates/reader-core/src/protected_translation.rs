use crate::Result;
use regex::Regex;
use std::{borrow::Cow, ops::Range, sync::LazyLock};
use unicode_normalization::UnicodeNormalization;

pub(crate) const MARKER_ERROR: &str = "Перекладач змінив або пропустив захищений фрагмент.";

static WEB_ADDRESSES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i:https?://)[^\s<>"«»“”‘’]+"#,
        r"|[A-Za-z0-9_%+.-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,63}\b",
    )).expect("web addresses")
});

const AMOUNT: &str = concat!(
    r"[+\-−]?(?:[0-9]{1,3}(?:\.[0-9]{3})+,[0-9]+",
    r"|[0-9]{1,3}(?:,[0-9]{3})+\.[0-9]+",
    r"|(?:[0-9]{1,3}(?:[ \u{a0}\u{202f}][0-9]{3})+|[0-9]+)(?:[,.][0-9]+)?)",
);
const SPACE: &str = r"[ \u{a0}\u{202f}]*";

static EURO_AMOUNTS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)(?P<before>{AMOUNT}){SPACE}(?:euros?|EUR|€)|(?:EUR|€){SPACE}(?P<after>{AMOUNT})",
    )).expect("euro amounts")
});

static PERCENTAGES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"{AMOUNT}{SPACE}[%‰]")).expect("percentages and per-mille figures")
});

static PAGE_REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"([pP]{1,2}\.|(?i:с\.|стор\.|сторінці|сторінка|сторінку|сторінки|сторінок|сторінках))[ \t\u{a0}\u{202f}]+",
        r"([0-9]{1,6}(?:[ \u{a0}\u{202f}]*[-–][ \u{a0}\u{202f}]*[0-9]{1,6})?)",
    )).expect("page references")
});

static FRENCH_MONTH_PHRASES: LazyLock<Regex> = LazyLock::new(|| {
    let months = crate::translation_dates::FRENCH_MONTHS.join("|");
    Regex::new(&format!(r"(?i)en[ \u{{a0}}\u{{202f}}]+({months})")).expect("French month phrases")
});

static ACRONYM_DEFINITIONS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(([A-Z]{3,8})\)").expect("parenthesized acronyms"));

fn standalone_matches<'a>(pattern: &'static Regex, source: &'a str) -> impl Iterator<Item = regex::Captures<'a>> {
    let mut addresses = web_address_spans(source).peekable();
    pattern.captures_iter(source).filter_map(move |matched| {
        let range = matched.get(0)?.range();
        let adjacent = |ch: char| ch.is_alphanumeric() || ch == '_';
        if source[..range.start].ends_with(adjacent) || source[range.end..].starts_with(adjacent) {
            return None;
        }
        while addresses.peek().is_some_and(|address| address.end <= range.start) {
            addresses.next();
        }
        if addresses.peek().is_some_and(|address| address.start < range.end) {
            return None;
        }
        Some(matched)
    })
}

fn euro_amounts(source: &str) -> impl Iterator<Item = (Range<usize>, &str)> + '_ {
    standalone_matches(&EURO_AMOUNTS, source).filter_map(|matched| {
        let amount = matched.name("before").or_else(|| matched.name("after"))?;
        Some((matched.get(0)?.range(), amount.as_str()))
    })
}

fn percentage_spans(source: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    standalone_matches(&PERCENTAGES, source).filter_map(|matched| matched.get(0).map(|value| value.range()))
}

fn page_references_in(source: &str, allow_ukrainian: bool) -> impl Iterator<Item = (Range<usize>, &str)> + '_ {
    standalone_matches(&PAGE_REFERENCES, source).filter_map(move |matched| {
        let range = matched.get(0)?.range();
        let rest = &source[range.end..];
        if (!allow_ukrainian && !matched.get(1)?.as_str().starts_with(['p', 'P'])) ||
            (rest.starts_with(['.', ',']) && rest[1..].starts_with(|ch: char| ch.is_ascii_digit()))
        {
            return None;
        }
        Some((range, matched.get(2)?.as_str()))
    })
}

pub(crate) fn page_references(source: &str) -> impl Iterator<Item = (Range<usize>, &str)> + '_ {
    page_references_in(source, false)
}

pub(crate) fn restore_unmarked_page_references(source: &str, translated: &str) -> Option<String> {
    let expected: Vec<_> = page_references(source).map(|(_, pages)| pages).collect();
    let actual: Vec<_> = page_references_in(translated, true).collect();
    if expected.is_empty() || expected.len() != actual.len() {
        return None;
    }
    let compact = |text: &str| text.chars().filter(|ch| !ch.is_whitespace()).map(|ch| {
        if ch == '-' { '–' } else { ch }
    }).collect::<String>();
    if !expected.iter().zip(&actual).all(|(source, (_, target))| compact(source) == compact(target)) {
        return None;
    }
    let mut output = String::new();
    let mut last = 0;
    for (pages, (span, _)) in expected.into_iter().zip(actual) {
        output.push_str(&translated[last..span.start]);
        output.push_str("с. ");
        output.push_str(pages);
        last = span.end;
    }
    output.push_str(&translated[last..]);
    Some(output)
}

pub(crate) fn contains_page_reference(source: &str) -> bool {
    page_references(source).next().is_some()
}

fn french_month_phrases(source: &str) -> impl Iterator<Item = (Range<usize>, &'static str)> + '_ {
    standalone_matches(&FRENCH_MONTH_PHRASES, source).filter_map(|matched| {
        let month = matched.get(1)?.as_str().to_lowercase();
        let translation = crate::translation_dates::ukrainian_month_phrase(&month)?;
        Some((matched.get(0)?.range(), translation))
    })
}

pub(crate) fn contains_french_month_phrase(source: &str) -> bool {
    french_month_phrases(source).next().is_some()
}

pub(crate) fn contains_percentage(source: &str) -> bool {
    percentage_spans(source).next().is_some()
}

pub(crate) fn contains_euro_amount(source: &str) -> bool {
    euro_amounts(source).next().is_some()
}

pub(crate) fn web_address_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    WEB_ADDRESSES.find_iter(text).filter_map(|address| {
        let raw = address.as_str();
        if !raw.contains("://") {
            return Some(address.range());
        }

        // Adjacent closing punctuation belongs to the prose unless it is paired inside the URL.
        let mut url = raw.trim_end_matches(['.', ',', ';', ':', '!', '?', '\'']);
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while url.ends_with(close) && url.matches(close).count() > url.matches(open).count() {
                url = url.trim_end_matches(close);
            }
        }
        let (_, tail) = url.split_once("://")?;
        let host = tail.split(['/', '?', '#']).next()?;
        host.chars().any(char::is_alphanumeric).then_some(address.start()..address.start() + url.len())
    })
}

pub(crate) fn web_address_crosses(text: &str, offset: usize) -> bool {
    if offset == 0 || offset == text.len()
        || text[..offset].ends_with(char::is_whitespace)
        || text[offset..].starts_with(char::is_whitespace)
    {
        return false;
    }
    let left = text[..offset].rfind(char::is_whitespace)
        .map_or(0, |index| index + text[index..].chars().next().unwrap().len_utf8());
    let right = text[offset..].find(char::is_whitespace)
        .map_or(text.len(), |index| offset + index);
    let relative = offset - left;
    web_address_spans(&text[left..right]).any(|span| span.start < relative && relative < span.end)
}

pub(crate) fn transform_outside_web_addresses(text: &str, transform: impl Fn(&str) -> String) -> String {
    let mut spans = web_address_spans(text).peekable();
    if spans.peek().is_none() {
        return transform(text);
    }
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    for span in spans {
        output.push_str(&transform(&text[offset..span.start]));
        output.push_str(&text[span.clone()]);
        offset = span.end;
    }
    output.push_str(&transform(&text[offset..]));
    output
}

struct Marker<'a> {
    token: String,
    replacement: Cow<'a, str>,
}

pub(crate) struct ProtectedText<'a> {
    input: Cow<'a, str>,
    markers: Vec<Marker<'a>>,
    only_protected: bool,
    page_references_only: bool,
}

impl<'a> ProtectedText<'a> {
    pub(crate) fn new(source: &'a str, target: &str) -> Self {
        Self::with_french_months(source, target, false)
    }

    pub(crate) fn with_french_months(source: &'a str, target: &str, french_months: bool) -> Self {
        let mut markers = Vec::new();
        let mut input = String::new();
        let mut offset = 0;
        let mut number = 1;
        let mut unprotected_letters = false;
        let mut source_upper = None;
        let mut page_reference_count = 0;
        let mut numeric: Vec<_> = percentage_spans(source).map(|range| {
            let replacement = Cow::Borrowed(&source[range.clone()]);
            (range, replacement)
        }).collect();
        let overlaps = |left: &Range<usize>, right: &Range<usize>| left.start < right.end && right.start < left.end;
        if target == "uk" {
            let amounts: Vec<_> = euro_amounts(source).filter(|(range, _)| {
                !numeric.iter().any(|(other, _)| overlaps(other, range))
            }).map(|(range, amount)| (range, Cow::Owned(format!("{amount} євро")))).collect();
            numeric.extend(amounts);
            let references: Vec<_> = page_references(source).filter(|(range, _)| {
                !numeric.iter().any(|(other, _)| overlaps(other, range))
            }).map(|(range, pages)| (range, Cow::Owned(format!("с. {pages}")))).collect();
            page_reference_count = references.len();
            numeric.extend(references);
        }
        let identifiers = protected_spans(source).filter(|(range, _)| {
            !numeric.iter().any(|(other, _)| overlaps(other, range))
        }).map(|(range, value)| {
            let replacement = value.filter(|_| target == "uk").unwrap_or(&source[range.clone()]);
            (range, Cow::Borrowed(replacement))
        });
        let mut spans: Vec<_> = identifiers.collect();
        spans.extend(numeric);
        if french_months {
            spans.extend(french_month_phrases(source).map(|(range, translated)| (range, Cow::Borrowed(translated))));
        }
        spans.sort_unstable_by_key(|(range, _)| range.start);
        for (range, replacement) in spans {
            let token = loop {
                let token = format!("ZXQ{number:03}QXZ");
                number += 1;
                if !source_upper
                    .get_or_insert_with(|| source.to_ascii_uppercase())
                    .contains(&token)
                {
                    break token;
                }
            };
            let plain = &source[offset..range.start];
            unprotected_letters |= plain.chars().any(char::is_alphabetic);
            input.push_str(plain);
            input.push_str(&token);
            markers.push(Marker { token, replacement });
            offset = range.end;
        }
        if markers.is_empty() {
            return Self {
                input: Cow::Borrowed(source),
                markers,
                only_protected: false,
                page_references_only: false,
            };
        }
        let tail = &source[offset..];
        unprotected_letters |= tail.chars().any(char::is_alphabetic);
        input.push_str(tail);
        Self {
            input: Cow::Owned(input),
            page_references_only: markers.len() == page_reference_count,
            markers,
            only_protected: !unprotected_letters,
        }
    }

    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.markers.is_empty()
    }

    pub(crate) fn only_protected(&self) -> bool {
        self.only_protected
    }

    pub(crate) fn has_only_page_reference_markers(&self) -> bool {
        self.page_references_only
    }

    pub(crate) fn detached_french_articles(&self) -> Option<String> {
        let mut input = self.input.to_string();
        for marker in &self.markers {
            for article in ["l’", "L’", "l'", "L'"] {
                input = input.replace(&format!("{article}{}", marker.token), &marker.token);
            }
        }
        (input != self.input).then_some(input)
    }

    pub(crate) fn restore(&self, output: String) -> Result<String> {
        if self.is_empty() {
            return Ok(output);
        }
        let mut seen = vec![false; self.markers.len()];
        let mut restored = String::with_capacity(output.len());
        let mut offset = 0;
        for range in ascii_tokens(&output) {
            let raw = &output[range.clone()];
            let Some(index) = self
                .markers
                .iter()
                .position(|marker| marker.token.eq_ignore_ascii_case(raw))
            else {
                if in_marker_namespace(raw) {
                    return Err(MARKER_ERROR.into());
                }
                continue;
            };
            if seen[index] {
                return Err(MARKER_ERROR.into());
            }
            seen[index] = true;
            restored.push_str(&output[offset..range.start]);
            let replacement = self.markers[index].replacement.as_ref();
            restored.push_str(replacement);
            offset = range.end;
            if replacement.ends_with('.') && output[offset..].strip_prefix('.')
                .is_some_and(|tail| !tail.starts_with('.'))
            {
                offset += 1;
            }
        }
        if !seen.into_iter().all(|seen| seen) {
            return Err(MARKER_ERROR.into());
        }
        restored.push_str(&output[offset..]);
        Ok(restored)
    }
}

pub(crate) fn contains_protected(source: &str) -> bool {
    protected_spans(source).next().is_some()
}

pub(crate) fn passage_token_spans(source: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<_> = protected_spans(source).map(|(span, _)| span)
        .chain(euro_amounts(source).map(|(span, _)| span))
        .chain(french_month_phrases(source).map(|(span, _)| span))
        .chain(page_references(source).map(|(span, _)| span))
        .chain(percentage_spans(source)).collect();
    spans.sort_unstable_by_key(|span| span.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(spans.len());
    for span in spans {
        if let Some(previous) = merged.last_mut().filter(|previous| previous.end > span.start) {
            previous.end = previous.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged
}

pub(crate) fn contains_dotted_acronym(source: &str) -> bool {
    token_spans(source, |word| is_dotted_acronym(word).then_some(None))
        .any(|(range, _)| is_dotted_acronym(&source[range]))
}

fn defined_acronyms(source: &str) -> impl Iterator<Item = &str> {
    standalone_matches(&ACRONYM_DEFINITIONS, source).filter_map(|matched| {
        let label = matched.get(1)?.as_str();
        let phrase = source[..matched.get(0)?.start()].rsplit(['.', '!', '?', ':', ';', '\n', '(', ')']).next()?;
        let mut words = phrase.rsplit(|ch: char| !ch.is_alphabetic()).filter(|word| !word.is_empty());
        for expected in label.chars().rev() {
            loop {
                let word = words.next()?;
                if word.nfkd().next()?.to_ascii_uppercase() == expected { break; }
                if !matches!(word, "l" | "d" | "de" | "des" | "du" | "et" | "la" | "le" | "les" | "à" |
                    "au" | "aux" | "pour" | "of" | "the" | "for" | "and") { return None; }
            }
        }
        Some(label)
    })
}

pub(crate) fn contains_defined_acronym(source: &str) -> bool {
    defined_acronyms(source).next().is_some()
}

pub(crate) fn without_identifiers(text: &str) -> Cow<'_, str> {
    let mut output = String::new();
    let mut offset = 0;
    for (range, _) in token_spans(text, |word| (is_identifier(word) || is_dotted_acronym(word)).then_some(None)) {
        output.push_str(&text[offset..range.start]);
        output.push(' ');
        offset = range.end;
    }
    if offset == 0 {
        return Cow::Borrowed(text);
    }
    output.push_str(&text[offset..]);
    Cow::Owned(output)
}

fn protected_spans(
    source: &str,
) -> impl Iterator<Item = (Range<usize>, Option<&'static str>)> + '_ {
    let defined: Vec<_> = defined_acronyms(source).collect();
    token_spans(source, move |word| {
        if let Some(translated) = ukrainian_acronym(word) {
            return Some(Some(translated));
        }
        (is_identifier(word) || in_marker_namespace(word) || is_dotted_acronym(word) || defined.contains(&word))
            .then_some(None)
    })
    .map(|(mut range, replacement)| {
        if is_dotted_acronym(&source[range.clone()]) {
            if let Some(tail) = source[range.end..].strip_prefix('.') {
                // Keep an unfamiliar abbreviation verbatim. For a known equivalent,
                // omit an interior abbreviation dot while leaving sentence punctuation.
                if replacement.is_none() || tail.trim_start().starts_with(char::is_lowercase) {
                    range.end += 1;
                }
            }
        }
        (range, replacement)
    })
}

fn token_spans<'a>(
    source: &'a str,
    classify: impl Fn(&str) -> Option<Option<&'static str>> + 'a,
) -> impl Iterator<Item = (Range<usize>, Option<&'static str>)> + 'a {
    let mut addresses = web_address_spans(source).peekable();
    let mut words = ascii_tokens(source).peekable();
    std::iter::from_fn(move || loop {
        if let Some(address) = addresses.peek() {
            if words.peek().is_none_or(|word| word.end > address.start) {
                let address = addresses.next()?;
                while words.peek().is_some_and(|word| word.start < address.end) {
                    words.next();
                }
                return Some((address, None));
            }
        }
        let range = words.next()?;
        if source[..range.start].ends_with(char::is_alphabetic)
            || source[range.end..].starts_with(char::is_alphabetic)
        {
            continue;
        }
        if let Some(replacement) = classify(&source[range.clone()]) {
            return Some((range, replacement));
        }
    })
}

fn in_marker_namespace(word: &str) -> bool {
    word.get(..3)
        .is_some_and(|start| start.eq_ignore_ascii_case("ZXQ"))
        || word
            .get(word.len().saturating_sub(3)..)
            .is_some_and(|end| end.eq_ignore_ascii_case("QXZ"))
}

fn ascii_tokens(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut offset = 0;
    std::iter::from_fn(move || {
        let start = offset + bytes[offset..].iter().position(u8::is_ascii_alphanumeric)?;
        offset = start + 1;
        while offset < bytes.len()
            && (bytes[offset].is_ascii_alphanumeric()
                || matches!(bytes[offset], b'-' | b'.' | b'_')
                    && bytes.get(offset + 1).is_some_and(u8::is_ascii_alphanumeric))
        {
            offset += 1;
        }
        Some(start..offset)
    })
}

fn ukrainian_acronym(word: &str) -> Option<&'static str> {
    let compact;
    let dotted = is_dotted_acronym(word);
    let word = if dotted {
        compact = word.replace('.', "");
        compact.as_str()
    } else {
        word
    };
    Some(match word {
        "ONU" => "ООН",
        "OTAN" | "NATO" => "НАТО",
        "UNESCO" => "ЮНЕСКО",
        "UNICEF" => "ЮНІСЕФ",
        "NASA" => "НАСА",
        "OMS" => "ВООЗ",
        "QI" if dotted => "IQ",
        "UE" => "ЄС",
        _ => return None,
    })
}

fn is_dotted_acronym(word: &str) -> bool {
    (3..=19).contains(&word.len())
        && word.contains('.')
        && word.split('.').all(|part| part.len() == 1 && part.as_bytes()[0].is_ascii_uppercase())
}

fn is_identifier(word: &str) -> bool {
    if word.len() > 24 || !word.as_bytes()[0].is_ascii_uppercase() {
        return false;
    }
    let prefix = word.bytes().take_while(u8::is_ascii_uppercase).count();
    prefix <= 5
        && word.bytes().any(|byte| byte.is_ascii_digit())
        && word.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_page_numbers_and_localizes_the_ukrainian_reference() {
        let source = "Voir p. 189 et pp. 12–14. Répondez ensuite.";
        let protected = ProtectedText::new(source, "uk");
        assert_eq!(protected.input(), "Voir ZXQ001QXZ et ZXQ002QXZ. Répondez ensuite.");
        assert_eq!(protected.restore("Див. ZXQ001QXZ і ZXQ002QXZ. Далі дайте відповідь.".into()).unwrap(),
            "Див. с. 189 і с. 12–14. Далі дайте відповідь.");
        assert!(protected.restore("Див. ZXQ001QXZ.".into()).is_err());
        assert!(ProtectedText::new("p.\u{202f}26", "uk").only_protected());
        assert_eq!(ProtectedText::new(source, "de").input(), source);
        assert!(!contains_page_reference("http://exemple.fr/p.26, chap. 2, p. 3,14"));
        let spans = passage_token_spans(source);
        assert_eq!(spans.into_iter().map(|span| &source[span]).collect::<Vec<_>>(), ["p. 189", "pp. 12–14"]);
        assert_eq!(restore_unmarked_page_references(source, "Див. p. 189 і с. 12-14.").as_deref(),
            Some("Див. с. 189 і с. 12–14."));
        assert_eq!(restore_unmarked_page_references(source, "Див. сторінку 189 і сторінки 12-14.").as_deref(),
            Some("Див. с. 189 і с. 12–14."));
        assert!(restore_unmarked_page_references(source, "Див. p. 190 і с. 12–14.").is_none());
        assert!(restore_unmarked_page_references(source, "Див. p. 189.").is_none());
    }

    #[test]
    fn protects_french_month_phrases_in_ukrainian_prose() {
        let source = "La participation a augmenté de 19,50 % en octobre.";
        let protected = ProtectedText::with_french_months(source, "uk", true);
        assert_eq!(protected.input(), "La participation a augmenté de ZXQ001QXZ ZXQ002QXZ.");
        assert_eq!(protected.restore("Рівень участі зріс ZXQ002QXZ на ZXQ001QXZ.".into()).unwrap(),
            "Рівень участі зріс у жовтні на 19,50 %.");
        assert!(!contains_french_month_phrase("en maillot ou https://example.org/en%20octobre"));
    }

    #[test]
    fn percentages_preserve_the_value_unit_and_multiplicity() {
        let source = "L’ONU indique −19,50\u{a0}%, 1\u{202f}250,5 ‰ et −19,50\u{a0}%.";
        let protected = ProtectedText::new(source, "uk");
        assert_eq!(protected.input(), "L’ZXQ001QXZ indique ZXQ002QXZ, ZXQ003QXZ et ZXQ004QXZ.");
        assert_eq!(protected.restore("ZXQ001QXZ повідомляє про ZXQ003QXZ, ZXQ002QXZ і ZXQ004QXZ.".into()).unwrap(),
            "ООН повідомляє про 1\u{202f}250,5 ‰, −19,50\u{a0}% і −19,50\u{a0}%.");
        assert!(protected.restore("ZXQ001QXZ повідомляє про ZXQ002QXZ і ZXQ003QXZ.".into()).is_err());
        let percentages: Vec<_> = percentage_spans(source).map(|span| &source[span]).collect();
        assert_eq!(percentages, ["−19,50\u{a0}%", "1\u{202f}250,5 ‰", "−19,50\u{a0}%"]);
        assert!(!contains_percentage("Voir https://example.org/19%20ou%20−1,5%."));
        assert!(!contains_percentage("Code AB19% affiché."));
        assert!(ProtectedText::new("−19,50 %", "en").only_protected());
    }

    #[test]
    fn explicit_euro_amounts_keep_their_value_precision_and_position_in_ukrainian() {
        let source = "A320-200: 19,50 euros, puis EUR 1\u{202f}250,00 et €5.";
        let protected = ProtectedText::new(source, "uk");
        assert_eq!(protected.input(), "ZXQ001QXZ: ZXQ002QXZ, puis ZXQ003QXZ et ZXQ004QXZ.");
        assert_eq!(protected.restore("ZXQ003QXZ, ZXQ004QXZ та ZXQ002QXZ за ZXQ001QXZ.".into()).unwrap(),
            "1\u{202f}250,00 євро, 5 євро та 19,50 євро за A320-200.");
        assert!(protected.restore("ZXQ001QXZ, ZXQ002QXZ та ZXQ004QXZ.".into()).is_err());
        assert_eq!(ProtectedText::new("19,50 euros", "en").input(), "19,50 euros");
        assert!(!contains_euro_amount("Euro 2024 a lieu en Europe."));
        assert!(!contains_euro_amount("https://example.org/cost/19-euros?value=EUR25"));
        assert!(contains_euro_amount("Une correction de −19,50 euros."));
        let standalone = ProtectedText::new("€1.250,00", "uk");
        assert_eq!(standalone.restore(standalone.input().to_owned()).unwrap(), "1.250,00 євро");
    }

    #[test]
    fn restores_abbreviations_and_identifiers_at_their_translated_positions() {
        let source = "CHAPITRE 2. L’ONU présente l’A320-200 à l’UNESCO.";
        let protected = ProtectedText::new(source, "uk");
        assert_eq!(
            protected.input(),
            "CHAPITRE 2. L’ZXQ001QXZ présente l’ZXQ002QXZ à l’ZXQ003QXZ."
        );
        assert_eq!(
            protected
                .restore("ZXQ003QXZ побачило ZXQ002QXZ на презентації ZXQ001QXZ.".into())
                .unwrap(),
            "ЮНЕСКО побачило A320-200 на презентації ООН."
        );
        for invalid in [
            "ZXQ001QXZ ZXQ002QXZ",
            "ZXQ001QXZ ZXQ001QXZ ZXQ002QXZ ZXQ003QXZ",
            "ZXQ001QXZ ZXQ002QXZ ZXQ003QXZ ZXQ004QXZ",
            "ZXQ001QXZ ZXQ002QXZ ZXQ003QXZ ZXQ004QX",
        ] {
            assert!(protected.restore(invalid.into()).is_err());
        }
        assert!(!contains_protected("CHAPITRE 2. M. Morrel arrive."));
    }

    #[test]
    fn source_tokens_survive_repetition_and_non_ukrainian_hops() {
        let protected = ProtectedText::new("NATO, NASA, NASA", "en");
        assert!(protected.only_protected());
        assert_eq!(
            protected
                .restore("zxq003qxz et ZXQ002QXZ avec ZXQ001QXZ".into())
                .unwrap(),
            "NASA et NASA avec NATO"
        );
        let collision = ProtectedText::new("ZXQ001QXZ et ONU", "uk");
        assert_eq!(collision.input(), "ZXQ002QXZ et ZXQ003QXZ");
        assert_eq!(
            collision.restore(collision.input().to_owned()).unwrap(),
            "ZXQ001QXZ et ООН"
        );
    }

    #[test]
    fn keeps_dotted_abbreviations_without_preserving_uppercase_headings() {
        let protected = ProtectedText::new("Le C.N.R.S. collabore avec l’O.M.S. aujourd’hui.", "uk");
        assert_eq!(protected.input(), "Le ZXQ001QXZ collabore avec l’ZXQ002QXZ aujourd’hui.");
        assert_eq!(
            protected.restore("Сьогодні ZXQ002QXZ співпрацює з ZXQ001QXZ.".into()).unwrap(),
            "Сьогодні ВООЗ співпрацює з C.N.R.S."
        );
        let known_at_end = ProtectedText::new("Avec l’O.M.S.", "uk");
        assert_eq!(known_at_end.restore("З ZXQ001QXZ.".into()).unwrap(), "З ВООЗ.");
        let foreign = ProtectedText::new("L’O.M.S. annonce le résultat.", "en");
        assert_eq!(
            foreign.restore("ZXQ001QXZ announces the result.".into()).unwrap(),
            "O.M.S. announces the result."
        );
        assert_eq!(ProtectedText::new("Le Q.I. varie.", "uk")
            .restore("ZXQ001QXZ змінюється.".into()).unwrap(), "IQ змінюється.");
        assert!(contains_dotted_acronym("Le C.N.R.S. publie le rapport."));
        assert!(!contains_dotted_acronym("CHAPITRE II. M. Morrel arrive."));
        assert!(!contains_dotted_acronym("Voir https://example.org/publications/A.B.C."));
    }

    #[test]
    fn preserves_explicitly_defined_acronyms_without_protecting_the_expansion() {
        let source = "L’École de santé publique (ESP) publie les résultats. Contactez l’ESP.";
        let protected = ProtectedText::new(source, "uk");
        assert_eq!(protected.input(),
            "L’École de santé publique (ZXQ001QXZ) publie les résultats. Contactez l’ZXQ002QXZ.");
        assert_eq!(protected.restore(
            "Школа громадського здоров’я (ZXQ001QXZ) публікує результати. Зверніться до ZXQ002QXZ.".into(),
        ).unwrap(), "Школа громадського здоров’я (ESP) публікує результати. Зверніться до ESP.");
        assert!(protected.restore("Школа громадського здоров’я публікує результати.".into()).is_err());
        assert_eq!(passage_token_spans(source).into_iter().map(|span| &source[span]).collect::<Vec<_>>(),
            ["ESP", "ESP"]);
        assert!(contains_defined_acronym("Centre national de la recherche scientifique (CNRS)"));
        for plain in ["CHAPITRE (PARIS)", "Lire les résultats (NOTE)", "Fin. Reprendre le travail (ESP)"] {
            assert!(!contains_defined_acronym(plain), "{plain}");
            assert_eq!(ProtectedText::new(plain, "uk").input(), plain);
        }
        assert!(!contains_defined_acronym("Voir https://exemple.fr/ecole(ESP)"));
    }

    #[test]
    fn protects_whole_addresses_without_consuming_prose_punctuation() {
        let url = "https://example.org/dix-neuf_(fr)?id=A320-200&n=2";
        let email = "A320+ONu@example.org";
        let source = format!("Consultez ({url}), ou écrivez à {email}.");
        let protected = ProtectedText::new(&source, "uk");
        assert_eq!(protected.input(), "Consultez (ZXQ001QXZ), ou écrivez à ZXQ002QXZ.");
        assert_eq!(protected.restore("Пишіть на ZXQ002QXZ або відкрийте (ZXQ001QXZ).".into()).unwrap(),
            format!("Пишіть на {email} або відкрийте ({url})."));
        assert_eq!(without_identifiers(&format!("Пишіть на {email}.")), "Пишіть на  .");
        assert_eq!(
            crate::translation::prepare_translation_input("fr", "uk", format!("Il y a dix-neuf exemples sur {url}.")),
            format!("Il y a 19 exemples sur {url}."),
        );
        let address = ProtectedText::new(url, "uk");
        assert!(address.only_protected());
        assert_eq!(address.restore(address.input().to_owned()).unwrap(), url);
    }

    #[test]
    fn address_punctuation_does_not_end_a_sentence_or_shift_offsets() {
        let text = "📖 Ouvrez https://example.org/édition?lang=fr&id=7. Revenez ensuite.";
        let split = text.find('?').unwrap() + 1;
        assert!(web_address_crosses(text, split));
        for sentences in [crate::segment(text, "doc"), crate::segment_pdf_with_titles(text, "pdf")] {
            assert_eq!(sentences.len(), 2);
            assert_eq!(sentences[0].text, "📖 Ouvrez https://example.org/édition?lang=fr&id=7.");
            let second = &sentences[1];
            assert_eq!(second.text, "Revenez ensuite.");
            assert_eq!(second.start_utf8, text.find("Revenez").unwrap());
            assert_eq!(second.start_utf16, text[..second.start_utf8].encode_utf16().count());
        }
    }
}
