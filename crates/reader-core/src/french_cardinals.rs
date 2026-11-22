const SMALL: [&str; 17] = [
    "zéro", "un", "deux", "trois", "quatre", "cinq", "six", "sept", "huit", "neuf", "dix",
    "onze", "douze", "treize", "quatorze", "quinze", "seize",
];

pub(crate) fn compound(text: &str) -> Option<usize> {
    let text = text.trim().to_lowercase().replace(['‐', '‑'], "-");
    let small = |word: &str| SMALL.iter().position(|&value| value == word);
    let pieces: Vec<_> = text.split('-').collect();
    let (base, rest) = match pieces.as_slice() {
        ["dix", word] => return small(word).filter(|&unit| unit >= 7 && unit <= 9).map(|unit| 10 + unit),
        ["quatre", "vingts"] => return Some(80),
        ["quatre", "vingt", rest @ ..] if !rest.is_empty() => (80, rest),
        [tens, rest @ ..] if !rest.is_empty() => {
            let base = ["vingt", "trente", "quarante", "cinquante", "soixante"]
                .iter().position(|&word| word == *tens)?;
            (20 + 10 * base, rest)
        }
        _ => return None,
    };
    let unit = match (base, rest) {
        (20..=60, ["et", "un"]) => 1,
        (60, ["et", "onze"]) => 11,
        (60 | 80, ["dix", word]) => 10 + small(word).filter(|&value| (7..=9).contains(&value))?,
        (60 | 80, [word]) => small(word).filter(|&value| (1..=16).contains(&value))?,
        (_, [word]) => small(word).filter(|&value| (2..=9).contains(&value))?,
        _ => return None,
    };
    Some(base + unit)
}

pub(crate) fn digits_in_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    for piece in text.split_inclusive(|ch: char| !ch.is_alphabetic() && !matches!(ch, '-' | '‐' | '‑')) {
        let token = piece.trim_end_matches(|ch: char| !ch.is_alphabetic() && !matches!(ch, '-' | '‐' | '‑'));
        if token.contains(['-', '‐', '‑']) {
            if let Some(number) = compound(token) {
                output.push_str(&number.to_string());
                output.push_str(&piece[token.len()..]);
                continue;
            }
        }
        output.push_str(piece);
    }
    output
}
