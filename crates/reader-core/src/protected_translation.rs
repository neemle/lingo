use crate::Result;
use regex::Regex;
use std::{borrow::Cow, ops::Range, sync::LazyLock};

pub(crate) const MARKER_ERROR: &str = "Перекладач змінив або пропустив захищений фрагмент.";

static WEB_ADDRESSES: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i:https?://)[^\s<>"«»“”‘’]+"#,
        r"|[A-Za-z0-9_%+.-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,63}\b",
    )).expect("web addresses")
});

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
    replacement: &'a str,
}

pub(crate) struct ProtectedText<'a> {
    input: Cow<'a, str>,
    markers: Vec<Marker<'a>>,
    only_protected: bool,
}

impl<'a> ProtectedText<'a> {
    pub(crate) fn new(source: &'a str, target: &str) -> Self {
        let mut markers = Vec::new();
        let mut input = String::new();
        let mut offset = 0;
        let mut number = 1;
        let mut unprotected_letters = false;
        let mut source_upper = None;
        for (range, value) in protected_spans(source) {
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
            let original = &source[range.clone()];
            let replacement = if target == "uk" {
                value.unwrap_or(original)
            } else {
                original
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
            };
        }
        let tail = &source[offset..];
        unprotected_letters |= tail.chars().any(char::is_alphabetic);
        input.push_str(tail);
        Self {
            input: Cow::Owned(input),
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
            restored.push_str(self.markers[index].replacement);
            offset = range.end;
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

pub(crate) fn without_identifiers(text: &str) -> Cow<'_, str> {
    let mut output = String::new();
    let mut offset = 0;
    for range in token_spans(text, |word| is_identifier(word).then_some(None)).map(|(range, _)| range) {
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
    token_spans(source, |word| {
        if let Some(translated) = ukrainian_acronym(word) {
            return Some(Some(translated));
        }
        (is_identifier(word) || in_marker_namespace(word)).then_some(None)
    })
}

fn token_spans(
    source: &str,
    classify: fn(&str) -> Option<Option<&'static str>>,
) -> impl Iterator<Item = (Range<usize>, Option<&'static str>)> + '_ {
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
    let word = if word.contains('.') {
        if word
            .split('.')
            .any(|part| part.len() != 1 || !part.as_bytes()[0].is_ascii_uppercase())
        {
            return None;
        }
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
        "UE" => "ЄС",
        _ => return None,
    })
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
}
