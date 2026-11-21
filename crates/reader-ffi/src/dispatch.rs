use crate::{
    gateway::{self, Store},
    wire::{Command, Error, Result},
};
use serde::Serialize;
use serde_json::Value;

fn value<T: Serialize>(result: std::result::Result<T, String>) -> Result<Value> {
    serde_json::to_value(result.map_err(gateway::domain)?)
        .map_err(|_| Error::new("serialization_error", "Cannot encode core response"))
}

pub fn execute(store: &mut Store, command: Command) -> Result<Value> {
    match command {
        Command::Open { .. } => Err(Error::new("invalid_handle", "Open requires handle zero")),
        Command::Snapshot => value(store.snapshot()),
        Command::Import {
            name,
            bytes,
            language,
        } => value(store.import(&name, &bytes, &language)),
        Command::ImportImage {
            name,
            bytes,
            language,
            lines,
        } => value(store.import_image(&name, &bytes, &language, lines)),
        Command::Position { id, position } => value(store.position(&id, position)),
        Command::SetBookmark {
            id,
            sentence_id,
            saved,
        } => value(store.set_bookmark(&id, &sentence_id, saved)),
        Command::SetReviewedTranslation {
            id,
            sentence_id,
            target,
            text,
        } => value(store.set_reviewed_translation(&id, &sentence_id, &target, &text)),
        Command::TranslationPackages => value(Ok(store.translation_packages())),
        Command::InstallTranslationPackage { path } => {
            value(store.install_translation_package(std::path::Path::new(&path)))
        }
        Command::TranslateSentence {
            id,
            sentence_id,
            target,
        } => value(
            store
                .translate_installed_sentence(&id, &sentence_id, &target)
                .map(|translated| {
                    translated.map(|result| {
                        serde_json::json!({
                            "text": result.segments[0],
                            "pivotLanguage": result.route.pivot_language(),
                        })
                    })
                }),
        ),
        Command::SaveWord { entry } => value(store.save_word(entry)),
        Command::Review {
            id,
            direction,
            grade,
        } => value(store.review(&id, &direction, grade)),
        Command::Original { id } => value(store.original(&id)),
        Command::Export => value(store.export()),
        Command::Restore { backup } => value(store.restore(backup)),
    }
}
