use std::sync::OnceLock;
use unicode_segmentation::UnicodeSegmentation;
use whatlang::{Detector, Lang};

const FRENCH_MANNER_OF_WALKING: &str = " de son pas souple et fort";
pub(crate) const INSTRUCTION_REVISION: &str = "fr-uk-instructions-v7";
pub(crate) const PUBLICATION_RULE_REVISION: &str = "fr-uk-publication-rules-v1";

/// Established Ukrainian forms for complete publication labels and names.
pub(crate) fn publication_term(source: &str) -> Option<String> {
    let source = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some(credit) = publication_credit(&source) {
        return Some(credit);
    }
    let normalized = source.to_lowercase().replace('’', "'");
    let (volume, title) = normalized.split_once(':').map_or((None, normalized.as_str()), |(prefix, title)| {
        let volume = prefix.trim().strip_prefix("tome ").filter(|number| {
            !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
        });
        (volume, title.trim())
    });
    if normalized.contains(':') && volume.is_none() { return None; }
    let translated = match title {
        "le comte de monte-cristo" => "Граф Монте-Крісто",
        "le prisonnier du château d'if" => "В’язень замку Іф",
        "alexandre dumas" => "Александр Дюма",
        "lire en français facile" => "Читання простою французькою",
        "français langue étrangère" => "Французька як іноземна",
        _ => return None,
    };
    Some(match volume {
        Some(number) => format!("Том {number}. {translated}"),
        None => translated.to_owned(),
    })
}

fn publication_credit(source: &str) -> Option<String> {
    let (label, name) = if let Some((label, name)) = source.split_once(':') {
        (label.trim_end(), name.trim_start())
    } else {
        let (_, rest) = source.split_once(' ')?;
        let (_, name) = rest.split_once(' ')?;
        (&source[..source.len() - name.len() - 1], name)
    };
    let credit = match label.to_lowercase().as_str() {
        "adaptation" | "adaptation de" | "adapté par" | "adaptée par" => "Адаптація",
        "illustrations" | "illustrations de" | "illustré par" | "illustrée par" => "Ілюстрації",
        "traduction" | "traduction de" | "traduit par" | "traduite par" => "Переклад",
        "texte" | "texte de" | "écrit par" | "écrite par" => "Текст",
        _ => return None,
    };
    let name_words: Vec<_> = name.trim_end_matches('.').split_whitespace().collect();
    let name_part = |part: &str| {
        let stem = part.strip_prefix("d’").or_else(|| part.strip_prefix("d'")).unwrap_or(part);
        stem.starts_with(char::is_uppercase)
            && (stem.chars().all(|ch| ch.is_alphabetic() || matches!(ch, '-' | '\'' | '’'))
                || stem.strip_suffix('.').is_some_and(|initial| initial.chars().count() == 1))
    };
    if !(2..=8).contains(&name_words.len()) || !name_part(name_words[0])
        || !name_part(name_words[name_words.len() - 1])
        || !name_words.iter().all(|part| {
            name_part(part) || matches!(*part, "de" | "des" | "du" | "la" | "le" | "van" | "von")
        })
    {
        return None;
    }
    Some(format!("{credit}: {name}"))
}

/// Make the action and its object explicit before passing short exercise commands to Marian.
pub(crate) fn clarify_french_instructions(text: &str) -> String {
    let mut clarified = String::with_capacity(text.len());
    for sentence in text.split_sentence_bounds() {
        if let Some(rewritten) = clarify_instruction(sentence) {
            clarified.push_str(&rewritten);
        } else {
            clarified.push_str(sentence);
        }
    }
    clarified
}

fn clarify_instruction(sentence: &str) -> Option<String> {
    let opening = sentence.trim_start_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '–' | '—' | '-' | '«' | '“' | '"')
    });
    let lead = &sentence[..sentence.len() - opening.len()];
    let first_word_end = opening.find(|ch: char| !ch.is_alphabetic()).unwrap_or(opening.len());
    let (verb, tail) = opening.split_at(first_word_end);
    let object = tail.trim_start();
    let after = |phrase: &str| {
        object.strip_prefix(phrase).filter(|rest| {
            !rest.starts_with(|ch: char| ch.is_alphanumeric() || matches!(ch, '-' | '\'' | '’'))
        })
    };
    if verb.eq_ignore_ascii_case("justifiez") && tail.starts_with(char::is_whitespace) {
        let subjects = [
            ("votre choix", "vous avez fait ce choix"),
            ("vos choix", "vous avez fait ces choix"),
            ("votre réponse", "vous avez choisi cette réponse"),
            ("vos réponses", "vous avez choisi ces réponses"),
        ];
        for (source, reason) in subjects {
            if let Some(suffix) = after(source) {
                return Some(format!("{lead}Expliquez pourquoi {reason}{suffix}"));
            }
        }
    }
    if verb.eq_ignore_ascii_case("cochez") {
        let end = |suffix: &str| suffix.chars().all(|ch| {
            ch.is_whitespace() || matches!(ch, '.' | '!' | '?' | '…' | '»' | '”' | '"')
        });
        if let Some(suffix) = end(object).then_some(tail).or_else(|| after("la case").filter(|suffix| end(suffix))) {
            return Some(format!("{lead}Sélectionnez la case à cocher{suffix}"));
        }
        if tail.starts_with(char::is_whitespace) &&
            ["la bonne réponse", "la réponse", "l'option", "l’option"].iter().any(|phrase| after(phrase).is_some()) {
            return Some(format!("{lead}Cochez la case correspondant à {object}"));
        }
    }
    None
}

pub(super) fn french_manner_of_walking(text: &str) -> Option<usize> {
    text.match_indices(FRENCH_MANNER_OF_WALKING)
        .find(|(start, phrase)| {
            !text[start + phrase.len()..].starts_with(char::is_alphabetic)
        })
        .map(|(start, _)| start)
}

pub(super) fn clarify_french_manner_of_walking(text: &str) -> Option<String> {
    let start = french_manner_of_walking(text)?;
    let mut clarified = text.to_owned();
    clarified.replace_range(
        start..start + FRENCH_MANNER_OF_WALKING.len(),
        " d'une démarche souple et forte",
    );
    Some(clarified)
}

pub(super) fn clearly_russian(text: &str) -> bool {
    let sample = outside_quotes(text);
    distinguish(&sample) == Some(Lang::Rus) || contains_russian_clause(&sample)
}

pub(super) fn clearly_ukrainian(text: &str) -> bool {
    let sample = outside_quotes(text);
    distinguish(&sample) == Some(Lang::Ukr) && !contains_russian_clause(&sample)
}

pub(super) fn clearly_foreign_prose(source: &str, output: &str) -> bool {
    use crate::{language_detection::detect, protected_translation::without_identifiers};

    let prose = outside_quotes(&without_identifiers(output));
    let has_cyrillic = |text: &str| text.chars().any(|ch| {
        ('\u{0400}'..='\u{052f}').contains(&ch) && ch.is_alphabetic()
    });
    let foreign = |text: &str| !has_cyrillic(text) && detect(text).is_some_and(|code| code != "uk");
    if has_cyrillic(&prose) {
        return prose.split_sentence_bounds().any(foreign);
    }

    // A short Latin heading or name can be valid on its own. When it is all
    // that remains of a confidently identified foreign passage, prose was lost.
    foreign(&prose) || (prose.chars().filter(|ch| ch.is_alphabetic()).take(8).count() >= 8 &&
        detect(&without_identifiers(source)).is_some_and(|code| code != "uk"))
}

fn contains_russian_clause(sample: &str) -> bool {
    // Words that distinguish ordinary Russian prose even when it uses only shared letters.
    // Require two independent cues in the same unquoted sentence to avoid flagging names.
    sample.split_sentence_bounds().any(|clause| {
        if distinguish(clause) == Some(Lang::Rus) { return true; }
        let lowercase = clause.to_lowercase();
        let mut cues = lowercase.unicode_words().filter(|word| {
            matches!(*word,
                "она" | "они" | "оно" | "этот" | "этого" | "эти" | "это" | "будет" |
                "будут" | "будешь" | "будем" | "сейчас" | "чтобы" | "очень" | "только" |
                "который" | "которая" | "которые" | "сказал" | "потому" | "сегодня" |
                "он" | "что" | "мне" | "нужно" | "его" | "мужчина" | "приходит" | "встречу" | "сэр" | "но"
            ) || ["ются", "ятся", "ется"].iter().any(|ending| word.ends_with(ending))
        });
        cues.next().is_some_and(|first| cues.any(|word| word != first))
    })
}

pub(super) fn repair_french_affirmative(source: &str, target: &str) -> Option<String> {
    let trim_prefix = |ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-');
    let source = source.trim_start_matches(trim_prefix).to_lowercase();
    let tail = source.strip_prefix("oui,")?.trim_start();
    let opening = target.trim_start_matches(trim_prefix);
    let suffix = opening.strip_prefix("Да,").or_else(|| opening.strip_prefix("да,"))?;
    let person = tail.strip_prefix("chez ").and_then(|rest|
        rest.strip_prefix("l’").or_else(|| rest.strip_prefix("l'"))
    ).map(|rest| rest.trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '!' | '?')));
    let addressed = suffix.trim_start();
    if let Some((preposition, noun)) = addressed.split_once(' ') {
        let noun = noun.trim_end_matches(['.', '!', '?']);
        if matches!(preposition, "в" | "у") && noun.ends_with("аторе") &&
            noun.chars().all(char::is_alphabetic) &&
            person.is_some_and(|word| word.ends_with("ateur") && word.chars().all(char::is_alphabetic))
        {
            return Some(format!("{}Так, в {}а{}", &target[..target.len() - opening.len()],
                noun.trim_end_matches('е'), &addressed[preposition.len() + 1 + noun.len()..]));
        }
    }
    let repaired = format!("{}Так,{suffix}", &target[..target.len() - opening.len()]);
    (!clearly_russian(&repaired)).then_some(repaired)
}

struct DialogueFrame<'a> {
    prefix: &'a str,
    body: &'a str,
    suffix: &'a str,
}

impl<'a> DialogueFrame<'a> {
    fn new(source: &'a str) -> Self {
        let source = source.trim();
        let opening = source.trim_start_matches(|ch: char| ch.is_whitespace()
            || matches!(ch, '–' | '—' | '-' | '«' | '“' | '„' | '"'));
        let body = opening.trim_end_matches(|ch: char| ch.is_whitespace()
            || matches!(ch, '.' | '!' | '?' | '…' | '»' | '”' | '"'));
        Self { prefix: &source[..source.len() - opening.len()], body, suffix: &opening[body.len()..] }
    }

    fn restore(&self, target: &str) -> String {
        let target = target.trim();
        let target = if self.prefix.is_empty() { target } else {
            target.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-' | '«' | '“' | '„' | '"'))
        };
        let target = if self.suffix.is_empty() { target } else {
            target.trim_end_matches(['.', '!', '?', '…', '»', '”', '"']).trim_end()
        };
        format!("{}{target}{}", self.prefix, self.suffix)
    }
}

/// Standalone polite formulas have a direct rendering without a model guess.
pub(super) fn french_dialogue_formula(source: &str) -> Option<String> {
    let frame = DialogueFrame::new(source);
    let body = frame.body.to_lowercase();
    let forms: Vec<_> = body.split(',').map(str::trim).collect();
    let address = |name| match name {
        "monsieur" => Some("пане"),
        "madame" => Some("пані"),
        _ => None,
    };
    let translated = match forms.as_slice() {
        [answer @ ("oui" | "non"), name, tail @ ..] if tail.is_empty() || *tail == ["mais"] => {
            let answer = if *answer == "oui" { "Так" } else { "Ні" };
            let conjunction = if tail.is_empty() { "" } else { ", але" };
            format!("{answer}, {}{conjunction}", address(*name)?)
        }
        ["je suis à vos ordres"] => "Я у вашому розпорядженні".into(),
        ["je suis à vos ordres", name] => format!("Я у вашому розпорядженні, {}", address(*name)?),
        _ => return None,
    };
    Some(frame.restore(&translated))
}

pub(super) struct FrenchDialogueRetry<'a> {
    frame: DialogueFrame<'a>,
    pub input: String,
    address: Option<&'static str>,
}

impl FrenchDialogueRetry<'_> {
    pub(super) fn ukrainian_clause(&self, target: &str) -> bool {
        if clearly_ukrainian(target) { return true; }
        if self.address.is_none() || clearly_russian(target) { return false; }
        let lower = target.to_lowercase();
        let mut cues = lower.unicode_words().filter(|word| matches!(*word, "мені" | "навіть" | "будь" | "будьмо" |
            "дякую" | "потрібно" | "годині" | "звідки" | "сьогодні"));
        cues.next().is_some_and(|first| cues.any(|word| word != first))
    }

    pub(super) fn restore(&self, target: &str) -> String {
        let Some(address) = self.address else { return self.frame.restore(target); };
        let target = DialogueFrame::new(target).body;
        let first = target.split_whitespace().next().unwrap_or_default();
        let clause = match first {
            "Я" | "Мені" | "Мене" | "Ми" | "Нам" | "Ви" | "Вам" | "Ти" | "Тобі" | "Це" | "Тут" =>
                format!("{}{}", first.to_lowercase(), &target[first.len()..]),
            _ => target.to_owned(),
        };
        self.frame.restore(&format!("{address}, {clause}"))
    }
}

fn french_initial_address(body: &str) -> Option<(&'static str, &str)> {
    let (address, clause) = body.split_once(',')?;
    let clause = clause.trim_start();
    if clause.is_empty() { return None; }
    let translated = match address.trim().to_ascii_lowercase().as_str() {
        "monsieur" => "Пане",
        "madame" | "mademoiselle" => "Пані",
        _ => return None,
    };
    Some((translated, clause))
}

pub(crate) fn has_french_initial_address(source: &str) -> bool {
    french_initial_address(DialogueFrame::new(source).body).is_some()
}

pub(super) fn french_dialogue_retry(source: &str) -> Option<FrenchDialogueRetry<'_>> {
    let frame = DialogueFrame::new(source);
    if frame.body.is_empty() { return None; }
    let leading = french_initial_address(frame.body);
    let rewritten = leading.map_or(frame.body, |(_, clause)| clause)
        .replace(" vient à la rencontre de ", " s'avance vers ");
    let mut words = rewritten.split_whitespace().peekable();
    let mut input = String::with_capacity(rewritten.len());
    while let Some(word) = words.next() {
        let word = if words.peek().is_some_and(|next| next.starts_with(char::is_uppercase)) {
            match word { "M." => "monsieur", "Mme" | "Mme." => "madame", "Mlle" | "Mlle." => "mademoiselle", _ => word }
        } else { word };
        if input.is_empty() && leading.is_some() {
            let mut chars = word.chars();
            input.extend(chars.next()?.to_uppercase());
            input.push_str(chars.as_str());
        } else {
            if !input.is_empty() { input.push(' '); }
            input.push_str(word);
        }
    }
    if input != frame.body && matches!(frame.suffix, "." | "!" | "?") {
        input.push_str(frame.suffix);
    }
    if input == source || crate::pdf_sentences::Layout::ClosingQuotes.segment(&input, "").len() != 1 {
        return None;
    }
    Some(FrenchDialogueRetry { frame, input, address: leading.map(|(address, _)| address) })
}

pub(crate) struct FrenchSpeechAttribution<'a> {
    frame: DialogueFrame<'a>,
    pub input: String,
}

impl FrenchSpeechAttribution<'_> {
    pub(super) fn without_dialogue_dash<'a>(&self, source: &'a str) -> &'a str {
        if matches!(self.frame.prefix.trim(), "-" | "–" | "—") {
            &source.trim()[self.frame.prefix.len()..]
        } else {
            source
        }
    }

    pub(super) fn restore_frame(&self, text: &str) -> String {
        self.frame.restore(text)
    }

    pub(super) fn recover(&self, baseline: &str, attribution: &str) -> Option<String> {
        let speech = DialogueFrame::new(baseline).body.trim_end_matches(',');
        let attribution = DialogueFrame::new(attribution).body;
        let folded_speech = speech.to_lowercase();
        let folded_attribution = attribution.to_lowercase();
        let attribution_words: Vec<_> = folded_attribution.unicode_words().collect();
        if speech.is_empty()
            || !attribution.starts_with(char::is_uppercase)
            || !clearly_ukrainian(attribution)
            || attribution_words.len() < 2
            || folded_speech.unicode_words().any(|word| attribution_words.contains(&word))
        {
            return None;
        }
        Some(self.frame.restore(&format!("{speech}, — {attribution}")))
    }
}

/// A sentence-final reporting clause with an inverted named subject.
pub(crate) fn french_speech_attribution(source: &str) -> Option<FrenchSpeechAttribution<'_>> {
    if source.len() > 768 { return None; }
    let frame = DialogueFrame::new(source);
    let (speech, attribution) = frame.body.rsplit_once(',')?;
    if speech.trim().is_empty() || frame.body.contains(['«', '»', '“', '”', '"']) {
        return None;
    }
    let attribution = attribution.trim_start();
    let verb_end = attribution.find(char::is_whitespace)?;
    let verb = &attribution[..verb_end];
    if !matches!(verb, "dit" | "répond" | "répondit" | "ajoute" | "ajouta" | "demande"
        | "demanda" | "murmure" | "murmura" | "s'exclame" | "s’exclame" | "reprit" | "reprend")
    {
        return None;
    }
    let tail = attribution[verb_end..].trim_start();
    let name_end = tail.find(|ch: char| ch.is_whitespace()).unwrap_or(tail.len());
    let name = &tail[..name_end];
    if !name.starts_with(char::is_uppercase) || !name.chars().all(|ch|
        ch.is_alphabetic() || matches!(ch, '-' | '\'' | '’'))
    {
        return None;
    }
    let tail = tail[name_end..].trim_start();
    let (word, rest) = tail.split_once(char::is_whitespace).unwrap_or((tail, ""));
    let (name, tail) = if word.starts_with(char::is_uppercase)
        && word.chars().all(|ch| ch.is_alphabetic() || ch == '-')
    {
        (format!("{name} {word}"), rest.trim_start())
    } else {
        (name.to_owned(), tail)
    };
    // Explicit simultaneity prevents the gerund from becoming a means of answering.
    let mut input = format!("{name} {verb}");
    if !tail.is_empty() {
        input.push(' ');
        if tail.starts_with("en ") { input.push_str("tout "); }
        input.push_str(tail);
    }
    input.push('.');
    Some(FrenchSpeechAttribution { frame, input })
}

fn distinguish(sample: &str) -> Option<Lang> {
    // An independent affirmative is distinctive even when the rest is made
    // up of shared vocabulary. Keep ambiguous words and quotations untouched.
    let opening = sample.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-'));
    let mut characters = opening.chars().flat_map(char::to_lowercase);
    if characters.next() == Some('д') && characters.next() == Some('а') &&
        characters.next().is_some_and(|ch| matches!(ch, ',' | '.' | '!' | '?' | '…'))
    {
        return Some(Lang::Rus);
    }
    if sample
        .unicode_words()
        .filter(|word| {
            word.chars()
                .any(|c| ('\u{0400}'..='\u{04ff}').contains(&c) && c.is_alphabetic())
        })
        .take(3)
        .count()
        < 3
    {
        return None;
    }

    let russian = sample.contains(['ы', 'э', 'ё', 'ъ', 'Ы', 'Э', 'Ё', 'Ъ']);
    let ukrainian = sample.contains(['і', 'ї', 'є', 'ґ', 'І', 'Ї', 'Є', 'Ґ']);
    let expected = match (russian, ukrainian) {
        (true, false) => Lang::Rus,
        (false, true) => Lang::Ukr,
        _ => return None,
    };
    let detected = whatlang::detect(sample)?;
    if detected.lang() == expected && detected.confidence() >= 0.45 { return Some(expected); }
    // Short dialogue can score poorly against all languages. Require a Russian-only
    // letter in a lowercase word before comparing the two closely related languages.
    if expected == Lang::Rus && sample.unicode_words().any(|word|
        word.starts_with(char::is_lowercase) && word.contains(['ы', 'э', 'ё', 'ъ']))
    {
        static DETECTOR: OnceLock<Detector> = OnceLock::new();
        let focused = DETECTOR.get_or_init(|| Detector::with_allowlist(vec![Lang::Rus, Lang::Ukr]));
        if focused.detect(sample).is_some_and(|info| info.lang() == Lang::Rus && info.confidence() >= 0.65) {
            return Some(Lang::Rus);
        }
    }
    None
}

fn outside_quotes(text: &str) -> String {
    let dialogue = text.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '–' | '—' | '-'))
        .trim_end_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '!' | '?' | '…'));
    if let Some(open) = dialogue.chars().next() {
        if let Some(quoted) = dialogue.strip_prefix(open).and_then(|inner|
            quote_closer(open).and_then(|close| inner.strip_suffix(close)))
        {
            // A quote occupying the whole fragment is dialogue, not an inline citation.
            return quoted.to_owned();
        }
    }
    let mut sample = String::with_capacity(text.len());
    let mut closer = None;
    for ch in text.chars() {
        if let Some(end) = closer {
            if ch == end {
                closer = None;
            }
            continue;
        }
        closer = quote_closer(ch);
        sample.push(if closer.is_some() { ' ' } else { ch });
    }
    sample
}

fn quote_closer(open: char) -> Option<char> {
    match open {
        '«' => Some('»'),
        '„' => Some('“'),
        '“' => Some('”'),
        '"' => Some('"'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restores_omitted_speaker_without_duplicating_an_existing_attribution() {
        let attribution =
            french_speech_attribution("— Je n’ai même pas dix-neuf ans, répond Dantès en rougissant.")
                .unwrap();
        assert_eq!(attribution.input, "Dantès répond tout en rougissant.");
        assert_eq!(
            attribution.without_dialogue_dash("— Je n’ai même pas dix-neuf ans, répond Dantès en rougissant."),
            "Je n’ai même pas dix-neuf ans, répond Dantès en rougissant."
        );
        let speech = "Мені навіть не 19 років.";
        let tail = "Дантес відповідає, почервонівши.";
        assert_eq!(
            attribution.recover(speech, tail).as_deref(),
            Some("— Мені навіть не 19 років, — Дантес відповідає, почервонівши.")
        );
        assert_eq!(attribution.recover("Мені навіть не 19 років, відповів Дантес.", tail), None);
        assert_eq!(attribution.recover(speech, "Дантес отвечает, покраснев."), None);
        assert!(french_speech_attribution("Je viendrai, Pierre, demain matin.").is_none());
        assert!(french_speech_attribution("Il part, dit-on, à midi.").is_none());
    }

    #[test]
    fn publication_credits_translate_the_role_and_preserve_the_name() {
        for (source, expected) in [
            ("  Illustré par\nAnne de La Roche  ", "Ілюстрації: Anne de La Roche"),
            ("Traduction\u{a0}: M. d’Aubigny.", "Переклад: M. d’Aubigny."),
            ("Texte de Julie Bernard", "Текст: Julie Bernard"),
            ("Adaptation : Vincent Leroger", "Адаптація: Vincent Leroger"),
        ] {
            assert_eq!(publication_term(source).as_deref(), Some(expected));
        }
        assert!(publication_term("Adapté par plusieurs personnes.").is_none());
        assert!(publication_term("Traduction : un travail difficile.").is_none());
    }

    #[test]
    fn makes_imperative_objects_explicit_in_french_exercises() {
        assert_eq!(
            clarify_french_instructions("Justifiez votre choix. Cochez. Cochez la bonne réponse."),
            "Expliquez pourquoi vous avez fait ce choix. Sélectionnez la case à cocher. Cochez la case correspondant à la bonne réponse."
        );
        assert_eq!(
            clarify_french_instructions("« Justifiez votre réponse en une phrase. »"),
            "« Expliquez pourquoi vous avez choisi cette réponse en une phrase. »"
        );
        for unchanged in ["Vérifiez votre choix.", "Vous justifiez votre choix.", "Justifiez la dépense.",
            "Cochez la case vide.", "Cochez-les."] {
            assert_eq!(clarify_french_instructions(unchanged), unchanged);
        }
    }

    #[test]
    fn clarifies_french_walking_manner_without_changing_the_action() {
        let input = "Le jeune homme part de son pas souple et fort donner ses ordres aux marins.";
        let expected = "Le jeune homme part d'une démarche souple et forte donner ses ordres aux marins.";
        assert_eq!(clarify_french_manner_of_walking(input).as_deref(), Some(expected));
        assert!(clarify_french_manner_of_walking("Il parle de son pas souple et fortuit.").is_none());
    }

    #[test]
    fn distinguishes_observed_wrong_language_from_ukrainian_candidate() {
        let wrong = "Мы не уедем до понедельника.";
        let right = "Ми не підемо до понеділка.";
        assert!(clearly_russian(wrong));
        assert!(!clearly_ukrainian(wrong));
        assert!(clearly_ukrainian(right));
        assert!(!clearly_russian(right));
    }

    #[test]
    fn detects_unquoted_russian_even_with_ukrainian_elsewhere() {
        let mixed = "Она будет настоящей женой капитана. Давай, сейчас. Ви мене більше не нудить.";
        assert!(clearly_russian(mixed));
        assert!(!clearly_ukrainian(mixed));
        assert!(!clearly_russian("Він процитував: «Она будет женой капитана»."));
        assert!(!clearly_russian("Вона буде дружиною капітана. Тепер можна йти."));
    }

    #[test]
    fn detects_russian_dialogue_even_when_words_share_the_ukrainian_alphabet() {
        for rejected in [
            "Мне нужно управлять лодкой.",
            "Они встречаются со своими друзьями.",
            "Мужчина приходит на встречу с мистером Морреллом.",
            "- Так, сэр, но...",
            "Я вас командую, сэр.",
            "— «Мне нужно управлять лодкой».",
            "Спочатку він мовчав. В Антона был брат. А потім пішов.",
        ] { assert!(clearly_russian(rejected), "{rejected}"); }

        for accepted in [
            "— «Мені треба керувати човном».",
            "Вони зустрічаються зі своїми друзями.",
            "Так, пане, але...",
            "Пан Моррел стояв коло трапа.",
            "Він написав: «Мне нужно управлять лодкой».",
            "Мэр Ёлкин прибув сьогодні.",
        ] { assert!(!clearly_russian(accepted), "{accepted}"); }
    }

    #[test]
    fn rejects_russian_affirmatives_in_short_ukrainian_translations() {
        assert!(clearly_russian("Да, в узурпаторе."));
        assert!(clearly_russian("— ДА!"));
        assert!(!clearly_russian("Так, в узурпатора."));
        assert!(!clearly_russian("Давня історія."));
        assert!(!clearly_russian("«Да!» — відповів він."));
        assert_eq!(french_dialogue_formula("— Oui, monsieur.").as_deref(), Some("— Так, пане."));
        assert!(french_dialogue_formula("Oui, monsieur travaille.").is_none());
    }

    #[test]
    fn preserves_french_dialogue_frames_and_only_rewrites_complete_formulas() {
        assert_eq!(french_dialogue_formula("— « Oui, monsieur, mais... »").as_deref(), Some("— « Так, пане, але... »"));
        assert_eq!(french_dialogue_formula("« Je suis à vos ordres, madame.").as_deref(), Some("« Я у вашому розпорядженні, пані."));
        assert!(french_dialogue_formula("Oui, monsieur, mais nous arrivons.").is_none());
        assert!(french_dialogue_formula("Je suis à vos ordres, monsieur. Il descend.").is_none());
        let retry = french_dialogue_retry("Un homme vient à la rencontre de M. Dupont.").unwrap();
        assert_eq!(retry.input, "Un homme s'avance vers monsieur Dupont.");
        assert_eq!(retry.restore("Чоловік наближається до пана Дюпона!"), "Чоловік наближається до пана Дюпона.");
        assert!(french_dialogue_retry("Une porte se ferme. Il part.").is_none());
        let retry = french_dialogue_retry("— « Monsieur, je n’ai même pas 19 ans. »").unwrap();
        assert_eq!(retry.input, "Je n’ai même pas 19 ans");
        assert_eq!(retry.restore("Мені навіть не 19."), "— « Пане, мені навіть не 19. »");
        assert!(retry.ukrainian_clause("Мені навіть не 19."));
        assert!(!retry.ukrainian_clause("Мне ещё нет 19 лет."));
        assert!(!has_french_initial_address("Monsieur Dupont travaille."));
    }

    #[test]
    fn abstains_on_short_ambiguous_and_mixed_evidence() {
        for text in [
            "Мы",
            "Москва",
            "Станція Метро",
            "Мэр Ёлкин прибув сьогодні.",
        ] {
            assert!(!clearly_russian(text), "{text}");
            assert!(!clearly_ukrainian(text), "{text}");
        }
        assert!(!clearly_russian(
            "Він написав: «Мы не уедем до понедельника»."
        ));
        assert!(clearly_ukrainian(
            "Він сьогодні написав у листі: «Мы не уедем до понедельника»."
        ));
    }
}
