use crate::french_cardinals::spoken_cardinal;
use std::borrow::Cow;
use unicode_segmentation::UnicodeSegmentation;

pub(crate) fn expand(text: &str) -> Cow<'_, str> {
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
            output.push_str(&text[copied..start]);
            output.push_str(&number);
            copied = end;
            let unit = following.trim_start_matches([' ', '\u{00a0}', '\u{202f}']);
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
