use reader_core::{
    translation::{
        resolve_route, ModelEdge, Route, TranslationProvider, TranslationRequest,
        TranslationSettings,
    },
    Result, Store,
};

fn model(source: &str, target: &str, revision: &str, quality: Option<u16>) -> ModelEdge {
    ModelEdge {
        source: source.into(),
        target: target.into(),
        revision: revision.into(),
        quality,
    }
}

#[test]
fn installed_direct_model_wins_and_quality_and_ties_are_deterministic() {
    let direct = model("fr", "uk", "a", Some(600));
    let mut installed = vec![
        model("fr", "en", "fe", Some(990)),
        model("en", "uk", "eu", Some(990)),
        model("fr", "uk", "z", Some(600)),
        model("fr", "uk", "better-name-lower-quality", Some(599)),
        direct.clone(),
    ];
    for _ in 0..2 {
        assert_eq!(
            resolve_route(&installed, "fr", "uk").unwrap(),
            Some(Route::Direct(direct.clone()))
        );
        installed.reverse();
    }
}

#[test]
fn pivot_prefers_measured_quality_uses_english_on_a_tie_and_never_adds_a_third_model() {
    let mut installed = vec![
        model("fr", "en", "fe", Some(950)),
        model("en", "uk", "eu", Some(600)),
        model("fr", "de", "fd", Some(800)),
        model("de", "uk", "du", Some(800)),
        model("uk", "pl", "up", Some(990)),
    ];
    let pivot = |models: &[ModelEdge]| resolve_route(models, "fr", "uk").unwrap().unwrap();
    assert_eq!(pivot(&installed).pivot_language(), Some("de"));
    installed[3].quality = Some(600);
    assert_eq!(pivot(&installed).pivot_language(), Some("en"));
    installed[1].quality = None;
    assert_eq!(pivot(&installed).pivot_language(), Some("de"));
    assert!(resolve_route(&installed, "fr", "pl").unwrap().is_none());
    assert!(resolve_route(&installed, "es", "uk").unwrap().is_none());
}

#[derive(Default)]
struct Stub {
    installed: Vec<ModelEdge>,
    calls: Vec<(String, Vec<String>, Option<String>)>,
    invalid_alignment: bool,
}

impl TranslationProvider for Stub {
    fn installed_models(&self) -> Result<Vec<ModelEdge>> {
        Ok(self.installed.clone())
    }

    fn translate(
        &mut self,
        edge: &ModelEdge,
        segments: &[String],
        context: Option<&str>,
        _settings: TranslationSettings,
    ) -> Result<Vec<String>> {
        self.calls.push((
            edge.revision.clone(),
            segments.to_vec(),
            context.map(str::to_owned),
        ));
        if self.invalid_alignment {
            return Ok(vec![]);
        }
        Ok(segments
            .iter()
            .map(|text| format!("{}: {text} Another sentence.", edge.target))
            .collect())
    }
}

#[test]
fn pivot_keeps_alignment_even_when_target_sentence_counts_change() {
    let mut provider = Stub {
        installed: vec![
            model("fr", "en", "one", None),
            model("en", "uk", "two", None),
        ],
        ..Default::default()
    };
    let input = ["Un.".into(), "Deux.".into()];
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.db");
    let mut store = Store::open(&path).unwrap();
    let request = |source, target, context| TranslationRequest {
        source,
        target,
        segments: &input,
        context,
        settings: TranslationSettings::default(),
    };
    let result = store
        .translate_aligned(&mut provider, request("fr", "uk", Some("Contexte.")))
        .unwrap()
        .unwrap();
    assert_eq!(result.route.pivot_language(), Some("en"));
    assert_eq!(
        result.segments,
        [
            "uk: en: Un. Another sentence. Another sentence.",
            "uk: en: Deux. Another sentence. Another sentence."
        ]
    );
    assert_eq!(provider.calls.len(), 3);
    assert_eq!(provider.calls[0].2.as_deref(), Some("Contexte."));
    assert_eq!(
        provider.calls[2].2.as_deref(),
        Some("en: Contexte. Another sentence.")
    );

    provider.calls.clear();
    drop(store);
    let mut store = Store::open(&path).unwrap();
    assert_eq!(
        store
            .translate_aligned(&mut provider, request("fr", "uk", Some("Contexte.")))
            .unwrap(),
        Some(result)
    );
    assert!(provider.calls.is_empty());
    let mut changed = request("fr", "uk", Some("Contexte."));
    changed.settings.beam_size = 1;
    store
        .translate_aligned(&mut provider, changed)
        .unwrap()
        .unwrap();
    assert_eq!(provider.calls.len(), 3);
    provider.calls.clear();
    provider.installed[1].revision = "two-new".into();
    store
        .translate_aligned(&mut provider, request("fr", "uk", Some("Contexte.")))
        .unwrap()
        .unwrap();
    assert_eq!(provider.calls.len(), 3);
    provider.calls.clear();
    assert!(store
        .translate_aligned(&mut provider, request("uk", "fr", None))
        .unwrap()
        .is_none());
    assert!(provider.calls.is_empty());

    let removed = provider.installed.pop().unwrap();
    assert!(store
        .translate_aligned(&mut provider, request("fr", "uk", Some("Contexte.")))
        .unwrap()
        .is_none());
    assert!(provider.calls.is_empty());
    provider.installed.push(removed);

    provider.invalid_alignment = true;
    assert!(store
        .translate_aligned(&mut provider, request("fr", "uk", None))
        .is_err());
    assert_eq!(provider.calls.len(), 1);
}
