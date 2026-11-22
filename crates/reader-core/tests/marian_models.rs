use reader_core::Store;
use std::{
    fs::File,
    path::{Path, PathBuf},
};
use zip::{write::SimpleFileOptions, ZipWriter};
const FILES: [&str; 5] = [
    "config.json",
    "source.spm",
    "target.spm",
    "vocab.json",
    "model.safetensors",
];

fn package(directory: &Path, archive_path: &Path) {
    let mut archive = ZipWriter::new(File::create(archive_path).unwrap());
    for file in FILES {
        archive
            .start_file(
                file,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        std::io::copy(&mut File::open(directory.join(file)).unwrap(), &mut archive).unwrap();
    }
    archive.finish().unwrap();
}

#[test]
#[ignore = "requires locally downloaded pinned Helsinki-NLP model files"]
fn installs_pinned_models_translates_end_to_end_and_reuses_cache() {
    let model_dir =
        PathBuf::from(std::env::var_os("LINGO_TEST_MODEL_DIR").expect("LINGO_TEST_MODEL_DIR"));
    let scratch = tempfile::tempdir().unwrap();
    let mut store = Store::open(&scratch.path().join("model-test.sqlite")).unwrap();
    assert_eq!(store.translation_packages().len(), 24);
    assert!(store
        .translation_packages()
        .iter()
        .all(|model| !model.files_present));
    let french = store
        .import("french.txt", b"Elle n'a pas vendu sa voiture.", "fr")
        .unwrap();
    let english = store
        .import("english.txt", b"She did not sell her car.", "en")
        .unwrap();
    let ukrainian = store
        .import(
            "ukrainian.txt",
            "Вона не продала свою машину.".as_bytes(),
            "uk",
        )
        .unwrap();

    assert!(store
        .translate_installed_sentence(&french.id, &french.sentences[0].id, "uk")
        .unwrap()
        .is_none());

    for (document, target, expected_words) in [
        (&french, "en", ["sell", "car"]),
        (&english, "uk", ["не", "машин"]),
        (&french, "uk", ["не", "машин"]),
        (&ukrainian, "fr", ["pas", "voiture"]),
        (&english, "fr", ["pas", "voiture"]),
    ] {
        let sentence = &document.sentences[0].id;
        if document.language == "fr" && target == "uk" {
            let pivot = store
                .translate_installed_sentence(&document.id, sentence, target)
                .unwrap()
                .unwrap();
            assert_eq!(pivot.route.pivot_language(), Some("en"));
            assert!(
                expected_words
                    .iter()
                    .all(|word| pivot.segments[0].contains(word)),
                "{pivot:?}"
            );
            println!("fr → en → uk: {}", pivot.segments[0]);
        }

        let archive = scratch
            .path()
            .join(format!("{}-{target}.zip", document.language));
        package(
            &model_dir.join(format!("{}-{target}", document.language)),
            &archive,
        );
        assert_eq!(
            store.install_translation_package(&archive).unwrap(),
            format!("{} → {target}", document.language)
        );
        assert!(
            store
                .translation_packages()
                .iter()
                .any(|model| model.source == document.language
                    && model.target == target
                    && model.files_present),
            "Installed package is missing from the local inventory"
        );
        let result = store
            .translate_installed_sentence(&document.id, sentence, target)
            .unwrap()
            .unwrap();
        let output = &result.segments[0];
        println!("{} → {target}: {output}", document.language);
        assert_eq!(result.route.pivot_language(), None);
        if target == "en" {
            assert!(output.contains("not") || output.contains("n't"), "{output}");
        }
        assert!(
            expected_words.iter().all(|word| output.contains(word)),
            "{output}"
        );
        assert_eq!(
            store
                .translate_installed_sentence(&document.id, sentence, target)
                .unwrap(),
            Some(result)
        );
    }
}
