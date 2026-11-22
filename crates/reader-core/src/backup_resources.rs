use crate::Store;
use serde::{Deserialize, Serialize};

/// Reinstallable resource identity only. Backups never embed model weights.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupResource {
    pub kind: String,
    pub id: String,
    pub revision: String,
}

impl BackupResource {
    fn new(kind: &str, id: impl Into<String>, revision: &str) -> Self {
        Self {
            kind: kind.into(),
            id: id.into(),
            revision: revision.into(),
        }
    }
}

impl Store {
    pub(crate) fn backup_resources(&self) -> Vec<BackupResource> {
        let mut resources = Vec::new();
        resources.extend(
            self.translation_packages()
                .into_iter()
                .filter(|p| p.files_present)
                .map(|p| {
                    BackupResource::new(
                        "translation",
                        format!("{}-{}", p.source, p.target),
                        p.revision,
                    )
                }),
        );
        resources.extend(
            self.optional_models()
                .into_iter()
                .filter(|p| p.files_present)
                .map(|p| BackupResource::new("optional", p.id, p.revision)),
        );
        resources.extend(
            self.knowledge_packages()
                .into_iter()
                .filter(|p| p.installed)
                .map(|p| BackupResource::new("grammar", p.language, p.revision)),
        );
        resources.extend(
            self.lexical_packages()
                .into_iter()
                .filter(|p| p.installed)
                .map(|p| BackupResource::new("lexical", p.language, p.revision)),
        );
        resources.sort_by(|a, b| (&a.kind, &a.id, &a.revision).cmp(&(&b.kind, &b.id, &b.revision)));
        resources.dedup_by(|a, b| a.kind == b.kind && a.id == b.id && a.revision == b.revision);
        resources
    }
}
