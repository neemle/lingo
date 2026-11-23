//! Fast, local source-language suggestions. Unsupported guesses stay unknown.

pub fn detect(text: &str) -> Option<&'static str> {
    detect_sample(bounded_sample(text))
}

fn bounded_sample(text: &str) -> &str {
    &text[..text.char_indices().nth(8000).map_or(text.len(), |(i, _)| i)]
}

fn detect_sample(sample: &str) -> Option<&'static str> {
    supported(reliable_language(sample)?)
}

fn reliable_language(sample: &str) -> Option<whatlang::Lang> {
    if sample
        .chars()
        .filter(|c| c.is_alphabetic())
        .take(32)
        .count()
        < 32
    {
        return None;
    }
    whatlang::detect(sample).filter(whatlang::Info::is_reliable).map(|info| info.lang())
}

fn supported(language: whatlang::Lang) -> Option<&'static str> {
    Some(match language.code() {
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

#[derive(serde::Serialize)]
pub struct ImportLanguageSample {
    pub language: Option<&'static str>,
    pub sample: String,
}

pub fn import_sample(text: &str) -> ImportLanguageSample {
    let sample = crate::protected_translation::without_identifiers(bounded_sample(text));
    let detected = reliable_language(&sample);
    ImportLanguageSample {
        language: detected.and_then(supported),
        sample: if detected.is_some() { String::new() } else { sample.into_owned() },
    }
}

pub fn document_import_sample(name: &str, bytes: &[u8]) -> ImportLanguageSample {
    import_sample(&crate::parse_file(name, bytes).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguous_document_sample_excludes_nonlinguistic_addresses() {
        let sample = document_import_sample(
            "sample.txt",
            b"http://example.org/lang/en?parameter=hello\nA320-200 Contact+FR@example.org",
        );
        assert!(sample.language.is_none());
        assert!(sample.sample.trim().is_empty());
    }
}
