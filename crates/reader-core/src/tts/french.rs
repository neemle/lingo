use crate::french_cardinals::spoken_cardinal;
use serde::Serialize;
use std::{borrow::Cow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

const INLINE_SPACES: [char; 3] = [' ', '\u{00a0}', '\u{202f}'];

pub(crate) fn expand(text: &str) -> Cow<'_, str> {
    let Some(references) = expand_page_references(text) else { return expand_words(text); };
    Cow::Owned(expand_words(&references).into_owned())
}

fn expand_page_references(text: &str) -> Option<String> {
    let mut edits = page_reference_edits(text).peekable();
    edits.peek()?;
    let mut output = String::with_capacity(text.len());
    let mut copied = 0;
    for (span, replacement) in edits {
        output.push_str(&text[copied..span.start]);
        output.push_str(replacement);
        copied = span.end;
    }
    output.push_str(&text[copied..]);
    Some(output)
}

fn page_reference_edits(text: &str) -> impl Iterator<Item = (Range<usize>, &'static str)> + '_ {
    crate::protected_translation::page_references(text).flat_map(move |(span, pages)| {
        let number_start = span.end - pages.len();
        let abbreviation = text[span.start..number_start].trim_end();
        let range = pages.split_once(['-', '–']);
        let plural = range.is_some() || abbreviation.eq_ignore_ascii_case("pp.");
        let label = (span.start..number_start, if plural { "pages " } else { "page " });
        let separator = range.map(|(first, last)| {
            let left = number_start + first.trim_end().len();
            let right = span.end - last.trim_start().len();
            (left..right, " à ")
        });
        std::iter::once(label).chain(separator)
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechReplacement {
    start_utf16: usize,
    end_utf16: usize,
    text: &'static str,
}

pub fn french_reference_speech_edits(text: &str) -> Vec<SpeechReplacement> {
    let mut offset = 0;
    let mut units = 0;
    page_reference_edits(text).map(|(span, replacement)| {
        let start_utf16 = units + text[offset..span.start].encode_utf16().count();
        let end_utf16 = start_utf16 + text[span.clone()].encode_utf16().count();
        offset = span.end;
        units = end_utf16;
        SpeechReplacement { start_utf16, end_utf16, text: replacement }
    }).collect()
}

fn expand_words(text: &str) -> Cow<'_, str> {
    let mut output = String::new();
    let mut copied = 0;
    for (start, token) in text.split_word_bound_indices() {
        if start < copied {
            continue;
        }
        let token = token.trim_end_matches(['\u{00a0}', '\u{202f}']);
        let end = start + token.len();
        let following = &text[end..];
        let (period, rest) = following.strip_prefix('.').map_or((0, following), |rest| (1, rest));
        let title = if rest.starts_with(char::is_whitespace)
            && rest.trim_start().starts_with(char::is_uppercase)
        {
            match token {
                "M" if period == 1 => Some("monsieur"),
                "MM" if period == 1 => Some("messieurs"),
                "Mme" => Some("madame"),
                "Mmes" => Some("mesdames"),
                "Mlle" => Some("mademoiselle"),
                "Mlles" => Some("mesdemoiselles"),
                _ => None,
            }
        } else {
            None
        };
        if let Some(title) = title {
            output.push_str(&text[copied..start]);
            output.push_str(title);
            copied = end + period;
            continue;
        }
        if grouped_number_edge(following.chars()) || grouped_number_edge(text[..start].chars().rev()) {
            continue;
        }
        let number = if token == "1er" {
            Some("premier".to_owned())
        } else {
            spoken_number(token)
        };
        if let Some(number) = number {
            let prefix = &text[copied..start];
            if text[..copied].ends_with(|ch: char| ch.is_ascii_digit() || matches!(ch, '%' | '‰'))
                && prefix.trim_matches(INLINE_SPACES) == "–"
            {
                output.push_str(" à ");
            } else if let Some((unsigned_prefix, sign)) = signed_prefix(prefix, &text[..start]) {
                output.push_str(unsigned_prefix);
                output.push_str(sign);
            } else {
                output.push_str(prefix);
            }
            output.push_str(&number);
            copied = end;
            let unit = following.trim_start_matches(INLINE_SPACES);
            if let Some(symbol @ ('%' | '‰')) = unit.chars().next() {
                output.push_str(if symbol == '%' { " pour cent" } else { " pour mille" });
                copied += following.len() - unit.len() + symbol.len_utf8();
            }
        }
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    output.push_str(&text[copied..]);
    Cow::Owned(output)
}

fn signed_prefix<'a>(prefix: &'a str, preceding: &str) -> Option<(&'a str, &'static str)> {
    let sign = prefix.chars().next_back()?;
    let spoken = match sign {
        '-' | '−' => "moins ",
        '+' => "plus ",
        _ => return None,
    };
    let unsigned_prefix = prefix.strip_suffix(sign)?;
    let before_sign = preceding.strip_suffix(sign)?;
    before_sign.chars().next_back()
        .is_none_or(|ch| ch.is_whitespace() || matches!(ch, '(' | '[' | '«' | ':' | '='))
        .then_some((unsigned_prefix, spoken))
}

fn spoken_number(token: &str) -> Option<String> {
    let (whole, fraction) = token.split_once(',').map_or((token, None), |(whole, fraction)| (whole, Some(fraction)));
    if !whole.bytes().all(|byte| byte.is_ascii_digit()) || (whole.len() > 1 && whole.starts_with('0')) {
        return None;
    }
    let mut words = spoken_cardinal(whole.parse().ok()?)?;
    if let Some(fraction) = fraction {
        if !(1..=6).contains(&fraction.len()) || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        words.push_str(" virgule");
        let significant = fraction.trim_start_matches('0');
        for _ in significant.len()..fraction.len() {
            words.push_str(" zéro");
        }
        if !significant.is_empty() {
            words.push(' ');
            words.push_str(&spoken_cardinal(significant.parse().ok()?)?);
        }
    }
    Some(words)
}

// Leave digit groups and times intact for the voice's numeric handling.
fn grouped_number_edge(mut adjacent: impl Iterator<Item = char>) -> bool {
    matches!(adjacent.next(), Some(':' | '\u{00a0}' | '\u{202f}'))
        && adjacent.next().is_some_and(|ch| ch.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_french_page_references_as_pages_and_ranges() {
        assert_eq!(expand("Consultez la fiche, p. 189. Voir aussi pp.\u{202f}12-14."),
            "Consultez la fiche, page cent quatre-vingt neuf. Voir aussi pages douze à quatorze.");
        assert_eq!(expand("(P. 26), puis pp. 35 – 36."),
            "(page vingt six), puis pages trente cinq à trente six.");
        assert_eq!(expand("Nous parlons de P. Durand."), "Nous parlons de P. Durand.");
    }

    #[test]
    fn reads_french_dates_and_honorifics_inside_narration() {
        assert_eq!(
            expand("Le 15 février 1815, M. Morrel attend Mme Danglars."),
            "Le quinze février mille huit cent quinze, monsieur Morrel attend madame Danglars."
        );
        assert_eq!(expand("Le 1er mai, Mlle Stangerson a 19 ans."),
            "Le premier mai, mademoiselle Stangerson a dix-neuf ans.");
        assert_eq!(expand("À 10:30, l'édition A2 coûte 12,50 €."), "À 10:30, l'édition A2 coûte douze virgule cinquante €.");
        assert_eq!(expand("Épreuve 2, fiche 004, lot 20\u{202f}000."), "Épreuve deux, fiche 004, lot 20\u{202f}000.");
        assert_eq!(expand("Il a écrit M. puis G."), "Il a écrit M. puis G.");
    }

    #[test]
    fn reads_decimal_figures_and_their_units_as_words() {
        assert_eq!(
            expand("Le taux est de 19,50\u{202f}% puis de 2,5‰, avec un écart de 0,05 % et de 0,00%."),
            "Le taux est de dix-neuf virgule cinquante pour cent puis de deux virgule cinq pour mille, avec un écart de zéro virgule zéro cinq pour cent et de zéro virgule zéro zéro pour cent."
        );
        assert_eq!(expand("Le lot 004, version 1.25, part à 10:30."), "Le lot 004, version 1.25, part à 10:30.");
        assert_eq!(expand("Budget 30 %."), "Budget trente pour cent.");
    }

    #[test]
    fn distinguishes_numeric_ranges_and_signs_from_dialogue() {
        assert_eq!(
            expand("— La plage est de 12–15 %, puis de −2,5 à +3,0 ‰. Il fait -20 °C."),
            "— La plage est de douze à quinze pour cent, puis de moins deux virgule cinq à plus trois virgule zéro pour mille. Il fait moins vingt °C."
        );
        assert_eq!(expand("Les pages 12 – 15. — 20 ans."), "Les pages douze à quinze. — vingt ans.");
        assert_eq!(expand("Le train 12-15 arrive à 10:30."), "Le train douze-quinze arrive à 10:30.");
    }

    #[test]
    fn retains_french_cardinal_groups_and_plural_forms() {
        for (number, expected) in [
            (0, "zéro"), (21, "vingt et un"), (71, "soixante et onze"), (80, "quatre-vingts"),
            (91, "quatre-vingt onze"), (200, "deux cents"), (81_200, "quatre-vingt un mille deux cents"),
            (280_000, "deux cent quatre-vingt mille"),
        ] {
            assert_eq!(spoken_cardinal(number).as_deref(), Some(expected));
        }
    }
}
