use crate::Result;
use std::{borrow::Cow, ops::Range};

pub(crate) const MARKER_ERROR: &str = "Перекладач змінив або пропустив захищене скорочення чи код.";

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
    for range in ascii_tokens(text).filter(|range| is_identifier(&text[range.clone()])) {
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
    ascii_tokens(source).filter_map(|range| {
        let word = &source[range.clone()];
        if source[..range.start].ends_with(char::is_alphabetic)
            || source[range.end..].starts_with(char::is_alphabetic)
        {
            return None;
        }
        if let Some(translated) = ukrainian_acronym(word) {
            return Some((range, Some(translated)));
        }
        (is_identifier(word) || in_marker_namespace(word)).then_some((range, None))
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
}
