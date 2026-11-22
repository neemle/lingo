//! Fast, local source-language suggestions. Unsupported guesses stay unknown.

pub fn detect(text: &str) -> Option<&'static str> {
    let sample = &text[..text.char_indices().nth(8000).map_or(text.len(), |(i, _)| i)];
    if sample
        .chars()
        .filter(|c| c.is_alphabetic())
        .take(32)
        .count()
        < 32
    {
        return None;
    }
    let info = whatlang::detect(sample).filter(whatlang::Info::is_reliable)?;
    Some(match info.lang().code() {
        "eng" => "en",
        "ukr" => "uk",
        "fra" => "fr",
        "deu" => "de",
        "spa" => "es",
        "ita" => "it",
        "por" => "pt",
        "pol" => "pl",
        "nld" => "nl",
        "ces" => "cs",
        "ron" => "ro",
        "swe" => "sv",
        _ => return None,
    })
}

pub fn detect_document(name: &str, bytes: &[u8]) -> Option<&'static str> {
    crate::parse_file(name, bytes)
        .ok()
        .as_deref()
        .and_then(detect)
}
