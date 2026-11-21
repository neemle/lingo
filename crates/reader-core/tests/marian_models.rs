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
    for (language, input) in [
        ("fr", "Elle n'a pas vendu sa voiture."),
        ("en", "She did not sell her car."),
    ] {
        let document = store
            .import("sample.txt", input.as_bytes(), language)
            .unwrap();
        let sentence = &document.sentences[0].id;
        assert!(store
            .translate_installed_sentence(&document.id, sentence, "uk")
            .unwrap()
            .is_none());
        let archive = scratch.path().join(format!("{language}-uk.zip"));
        package(&model_dir.join(format!("{language}-uk")), &archive);
        assert_eq!(
            store.install_translation_package(&archive).unwrap(),
            format!("{language} → uk")
        );
        let result = store
            .translate_installed_sentence(&document.id, sentence, "uk")
            .unwrap()
            .unwrap();
        let output = &result.segments[0];
        println!("{language} → uk: {output}");
        assert_eq!(result.route.pivot_language(), None);
        assert!(
            output.contains("не") && output.contains("машин"),
            "{output}"
        );
        assert_eq!(
            store
                .translate_installed_sentence(&document.id, sentence, "uk")
                .unwrap(),
            Some(result)
        );
    }
}
