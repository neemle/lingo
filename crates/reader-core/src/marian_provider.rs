use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::marian::{Config, MTModel};
use rusqlite::{Connection, OpenFlags};
use sentencepiece_rs::SentencePieceProcessor;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    model_storage::{
        activate_verified_package, copy_and_hash, copy_and_hash_checked, lock_package, model_error,
        package_directory, remove_package_directory, restore_verified_package, write_verified_package_zip,
    },
    resources::RESOURCES,
    space::{ensure_capacity, PACKAGE_OVERHEAD},
    translation::{
        cached_translation, route_readiness, search_cached_translation, sentence_input,
        translate_paragraph, translate_pdf_page, AlignedTranslation, CachedTranslation, ModelEdge,
        PageBoundaries, ParagraphTranslation, ParagraphUnit, SelectedTranslation, TranslationProvider, TranslationReadiness, TranslationRequest,
        TranslationRouteStatus, TranslationSearchPage, TranslationSelection, TranslationSettings,
    },
    Result, Store,
};

#[path = "marian_decoder.rs"]
mod decoder;
#[path = "marian_validation.rs"]
mod probe_evidence;
#[path = "marian_rerank.rs"]
mod rerank;
#[path = "marian_time.rs"]
mod time;

mod catalog {
    include!("marian_catalog.rs");
}

const ASSETS: [&str; 5] = [
    "config.json",
    "source.spm",
    "target.spm",
    "vocab.json",
    "model.safetensors",
];
const MAX_ARCHIVE_BYTES: u64 = 400_000_000;

fn contextual_sentences(text: &str) -> impl Iterator<Item = &str> {
    text.unicode_sentences()
        .map(str::trim)
        .filter(|sentence| !sentence.is_empty())
        .take(4)
}

fn supports_contextual_recovery(text: &str, sentence_count: usize) -> bool {
    text.len() <= 768 && (2..=3).contains(&sentence_count)
}

pub(crate) fn may_recover_group(text: &str) -> bool {
    text.len() <= 768 && supports_contextual_recovery(text, contextual_sentences(text).count())
}

pub(crate) fn download_manifest(source: &str, target: &str) -> Result<crate::model_transfer::DownloadManifest> {
    let package = catalog::PACKAGES.iter()
        .find(|model| model.source == source && model.target == target)
        .ok_or("Для цієї мовної пари немає пакета.")?;
    Ok(crate::model_transfer::DownloadManifest {
        revision: package.revision,
        assets: ASSETS.iter().enumerate().map(|(index, &name)| crate::model_transfer::DownloadAsset {
            name,
            url: format!("https://huggingface.co/Helsinki-NLP/opus-mt-{}/resolve/{}/{name}", package.repository, package.revision),
            size: package.sizes[index],
            sha256: package.hashes[index],
        }).collect(),
    })
}

struct Package {
    pub source: &'static str,
    pub target: &'static str,
    pub repository: &'static str,
    pub license: &'static str,
    pub target_prefix: Option<&'static str>,
    pub revision: &'static str,
    pub sizes: [u64; 5],
    pub hashes: [&'static str; 5],
}

impl Package {
    fn edge(&self) -> ModelEdge {
        ModelEdge {
            source: self.source.into(),
            target: self.target.into(),
            revision: self.revision.into(),
            quality: None,
        }
    }
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPackageStatus {
    pub source: &'static str,
    pub target: &'static str,
    pub repository: &'static str,
    pub license: &'static str,
    pub files_present: bool,
    pub has_local_files: bool,
    pub has_recovery_copy: bool,
    pub revision: &'static str,
    pub assets: Vec<TranslationAsset>,
    pub validation: Option<TranslationPackageValidationResult>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationAsset {
    pub name: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

struct MarianProvider<'a> {
    directory: &'a Path,
    background: bool,
}

/// An installed translation package integrity check independent of the store connection.
pub struct TranslationPackageVerification {
    directory: PathBuf,
    package: &'static Package,
}

impl TranslationPackageVerification {
    pub fn run(self) -> Result<()> {
        if !self.directory.is_dir() {
            return Err("Пакет перекладу не встановлено.".into());
        }
        let provider = MarianProvider::interactive(&self.directory);
        let _lock = provider.lock(self.package, false)?;
        let package_path = provider.read_path(self.package);
        if !package_directory(&package_path)? {
            return Err("Пакет перекладу не встановлено.".into());
        }
        verify_files(&package_path, self.package, &ASSETS, &|| Ok(()))
    }
}

/// A direct-translation quality probe independent of the store connection.
pub struct TranslationPackageValidation {
    directory: PathBuf,
    package: &'static Package,
    samples: [&'static str; 4],
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationPackageValidationResult {
    pub source: &'static str,
    pub target: &'static str,
    pub repository: &'static str,
    pub revision: &'static str,
    pub input: &'static str,
    pub output: String,
    /// Character overlap on the bundled reference sentences, from 0 to 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<u16>,
}

impl TranslationPackageValidation {
    pub fn run(self) -> Result<TranslationPackageValidationResult> {
        let mut provider = MarianProvider::interactive(&self.directory);
        if !self.directory.is_dir() {
            return Err("Пакет перекладу не встановлено або він неповний.".into());
        }
        let (package_path, before) = {
            let _lock = provider.lock(self.package, false)?;
            let package_path = provider.read_path(self.package);
            let before = probe_evidence::fingerprint(&package_path, self.package)
                .ok_or("Пакет перекладу не встановлено або він неповний.")?;
            (package_path, before)
        };
        let segments = self.samples.map(str::to_owned);
        let outputs = provider.translate(
            &self.package.edge(),
            &segments,
            None,
            TranslationSettings::default(),
        )?;
        let quality = probe_evidence::score(self.package.target, &outputs)
            .ok_or("Модель повернула неповний або завеликий пробний переклад.")?;
        let mut outputs = outputs.into_iter();
        let output = outputs.next().ok_or("Модель не повернула пробний переклад.")?;
        if !contains_book_term(self.package.target, &output) {
            return Err("У пробному перекладі немає очікуваного слова цільовою мовою.".into());
        }

        let validation = TranslationPackageValidationResult {
            source: self.package.source,
            target: self.package.target,
            repository: self.package.repository,
            revision: self.package.revision,
            input: self.samples[0],
            output,
            quality: Some(quality),
        };

        // The translation acquires its own shared lock. Only attach this
        // result to the package generation observed before that inference.
        let _lock = provider.lock(self.package, true)?;
        let current = provider.read_path(self.package);
        let fingerprint = probe_evidence::fingerprint(&current, self.package);
        if current != package_path || fingerprint.as_ref() != Some(&before) {
            return Err("Пакет перекладу змінився під час перевірки.".into());
        }
        probe_evidence::write(&current, &validation, outputs.collect(), before)?;
        Ok(validation)
    }
}

fn contains_book_term(language: &str, output: &str) -> bool {
    output.unicode_words().any(|word| {
        let word = word.to_lowercase();
        match language {
            "en" => matches!(word.as_str(), "book" | "books"),
            "fr" => matches!(word.as_str(), "livre" | "livres" | "bouquin" | "bouquins"),
            "uk" => word.starts_with("книг") || word.starts_with("книж"),
            "de" => matches!(word.as_str(), "buch" | "bücher" | "büchern"),
            "es" => matches!(word.as_str(), "libro" | "libros"),
            "it" => matches!(word.as_str(), "libro" | "libri" | "volume" | "volumi"),
            "pt" => matches!(word.as_str(), "livro" | "livros"),
            "pl" => word.starts_with("książ") || word.starts_with("księg"),
            "nl" => matches!(word.as_str(), "boek" | "boeken"),
            "cs" => word.starts_with("knih") || word.starts_with("kníž"),
            "ro" => matches!(word.as_str(), "carte" | "cartea" | "cărți" | "cărţile" | "cărțile"),
            "sv" => matches!(word.as_str(), "bok" | "boken" | "böcker" | "böckerna"),
            _ => false,
        }
    })
}

/// Owns the model root so package maintenance can run independently of the store connection.
pub struct TranslationPackageFiles {
    directory: PathBuf,
}

impl TranslationPackageFiles {
    pub(crate) fn install_downloaded(
        &self, source: &str, target: &str, downloaded: &Path,
    ) -> Result<()> {
        let package = catalog::PACKAGES.iter()
            .find(|model| model.source == source && model.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let provider = MarianProvider::interactive(&self.directory);
        fs::create_dir_all(&self.directory).map_err(model_error)?;
        let _staging = lock_package(&self.directory.join(".install.lock"), true)?;
        if provider.active_package_verified(package)? {
            return Ok(());
        }
        let total = package.sizes.iter().sum::<u64>() + PACKAGE_OVERHEAD;
        ensure_capacity([(&self.directory, total)])?;
        let stage = tempfile::tempdir_in(&self.directory).map_err(model_error)?;
        for (index, asset) in ASSETS.iter().enumerate() {
            let mut output = File::create(stage.path().join(asset)).map_err(model_error)?;
            copy_verified_asset(downloaded, package, index, asset, &mut output, &|| Ok(()))?;
            output.sync_all().map_err(model_error)?;
        }
        provider.activate_staged(package, stage.path())
    }

    pub fn remove(&self, source: &str, target: &str) -> Result<bool> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let provider = MarianProvider::interactive(&self.directory);
        if !self.directory.is_dir() {
            return Ok(false);
        }
        let _lock = provider.lock(package, true)?;
        let active = provider.path(package);
        let removed = provider.removed_path(package);
        let mut cleared_previous = remove_package_directory(&removed)?;
        cleared_previous |= remove_package_directory(&provider.previous_path(package))?;
        if !package_directory(&active)? {
            return Ok(cleared_previous);
        }
        match fs::rename(&active, &removed) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(cleared_previous),
            Err(error) => return Err(model_error(error)),
        }
        remove_package_directory(&removed)
            .map_err(|error| format!("Пакет відключено. Не вдалося прибрати всі файли. {error}"))?;
        Ok(true)
    }

    pub fn rollback(&self, source: &str, target: &str) -> Result<()> {
        let package = catalog::PACKAGES.iter()
            .find(|entry| entry.source == source && entry.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let provider = MarianProvider::interactive(&self.directory);
        if !package_directory(&self.directory)? {
            return Err("Збережена копія пакета відсутня.".into());
        }
        let _lock = provider.lock(package, true)?;
        for candidate in provider.recovery_paths(package) {
            if package_directory(&candidate)?
                && verify_files(&candidate, package, &ASSETS, &|| Ok(())).is_ok()
            {
                return restore_verified_package(&candidate, &provider.path(package));
            }
        }
        Err("Немає збереженої копії, що відповідає контрольним сумам каталогу.".into())
    }

    pub fn export(&self, source: &str, target: &str, archive_path: &Path) -> Result<()> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        if !package_directory(&self.directory)? {
            return Err("Пакет перекладу не встановлено.".into());
        }
        let provider = MarianProvider::interactive(&self.directory);
        let _lock = provider.lock(package, false)?;
        let installed = provider.read_path(package);
        if !package_directory(&installed)? {
            return Err("Пакет перекладу не встановлено.".into());
        }

        let limit = (package.sizes.iter().sum::<u64>() + PACKAGE_OVERHEAD).min(MAX_ARCHIVE_BYTES);
        write_verified_package_zip(archive_path, limit, ASSETS, |index, asset, output| {
            copy_verified_asset(&installed, package, index, asset, output, &|| Ok(()))
        })
    }

    pub fn install(&self, archive_path: &Path) -> Result<String> {
        let file = File::open(archive_path).map_err(model_error)?;
        if file.metadata().map_err(model_error)?.len() > MAX_ARCHIVE_BYTES {
            return Err("Архів мовного пакета завеликий".into());
        }
        let mut archive = zip::ZipArchive::new(file).map_err(model_error)?;
        if archive.len() != ASSETS.len() {
            return Err("Архів має містити п’ять файлів мовного пакета".into());
        }
        let mut sizes = [0; ASSETS.len()];
        for (index, asset) in ASSETS.iter().enumerate() {
            let entry = archive.by_name(asset).map_err(model_error)?;
            sizes[index] = entry.size();
        }
        let (_, config_hash) = copy_and_hash(
            archive.by_name("config.json").map_err(model_error)?,
            io::sink(),
            asset_limit("config.json"),
        )?;
        let mut candidates: Vec<_> = catalog::PACKAGES
            .iter()
            .filter(|model| model.sizes == sizes && model.hashes[0] == config_hash)
            .collect();
        if candidates.is_empty() {
            return Err("Версію мовного пакета не підтримано".into());
        }
        let provider = MarianProvider::interactive(&self.directory);
        fs::create_dir_all(&self.directory).map_err(model_error)?;
        // Only one install per model root can reserve space and stage an archive at a time.
        // Package mutations still linearize independently at the package lock.
        let _staging_lock = lock_package(&self.directory.join(".install.lock"), true)?;
        ensure_capacity([(
            &self.directory,
            sizes.iter().sum::<u64>() + PACKAGE_OVERHEAD,
        )])?;
        let stage = tempfile::tempdir_in(&self.directory).map_err(model_error)?;
        for (index, asset) in ASSETS.iter().enumerate() {
            let input = archive.by_name(asset).map_err(model_error)?;
            let mut output = File::create(stage.path().join(asset)).map_err(model_error)?;
            let (size, digest) = copy_and_hash(input, &mut output, asset_limit(asset))?;
            candidates.retain(|model| model.sizes[index] == size && model.hashes[index] == digest);
            if candidates.is_empty() {
                return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
            }
            output.sync_all().map_err(model_error)?;
        }
        let package = candidates[0];
        if !provider.active_package_verified(package)? {
            provider.activate_staged(package, stage.path())?;
        }
        Ok(candidates
            .iter()
            .map(|entry| format!("{} → {}", entry.source, entry.target))
            .collect::<Vec<_>>()
            .join(", "))
    }
}

/// Owned paths for opening an independent connection to a library.
pub struct StoreWorker {
    database: PathBuf,
    model_directory: PathBuf,
    optional_model_directory: PathBuf,
    knowledge_directory: PathBuf,
}

impl StoreWorker {
    fn open(self, mode: OpenFlags) -> Result<Store> {
        let connection =
            Connection::open_with_flags(&self.database, mode | OpenFlags::SQLITE_OPEN_NO_MUTEX)
                .map_err(|error| error.to_string())?;
        connection.pragma_update(None, "foreign_keys", true)
            .map_err(|error| error.to_string())?;
        Ok(Store {
            connection,
            model_directory: self.model_directory,
            optional_model_directory: self.optional_model_directory,
            knowledge_directory: self.knowledge_directory,
            file_original_directory: crate::file_originals::directory(&self.database),
            active_pdf_ocr: None,
        })
    }

    pub fn complete_intake(
        self, id: &str, result: crate::intake::IntakeImport,
    ) -> Result<crate::intake::IntakeItem> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.complete_intake(id, result)
    }

    pub fn discard_intake(self, id: &str) -> Result<bool> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.discard_intake(id)
    }

    pub fn analyze_linguistics(
        self, id: &str, sentence_id: &str, revision: &str, scope: crate::knowledge::AnalysisScope,
    ) -> Result<serde_json::Value> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .cached_linguistic_analysis(id, sentence_id, revision, scope)
    }

    pub fn analyze_detached_pdf(
        self, id: &str, page: u16, request: crate::knowledge::DetachedPageAnalysis,
    ) -> Result<serde_json::Value> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .cached_detached_pdf_analysis(id, page, request)
    }

    pub fn prefetch_linguistics(
        self, id: &str, anchor_id: &str, revision: &str, step: i8, page: Option<u16>,
    ) -> Result<()> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .prefetch_linguistics(id, anchor_id, revision, step, page)
    }

    pub fn translate_sentence(
        self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .translate_installed_sentence(document_id, sentence_id, target)
    }

    pub fn translate_dictionary_gloss_to(
        self, id: &str, source: &str, gloss: &str, target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.translate_dictionary_gloss_to(id, source, gloss, target)
    }

    pub fn translate_dictionary_glosses(
        self, id: &str, source: &str, target: &str, glosses: &[String],
    ) -> Result<Vec<Option<String>>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .translate_dictionary_glosses(id, source, target, glosses)
    }

    pub fn translate_selection(
        self, id: &str, target: &str, selection: &TranslationSelection,
    ) -> Result<Option<SelectedTranslation>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.translate_selected_text(id, target, selection)
    }

    pub fn lookup_detached_pdf_word(
        self, id: &str, page: u16, start_utf16: usize, text: &str,
    ) -> Result<Option<crate::knowledge::WordLookup>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_ONLY)?.lookup_detached_pdf_word(id, page, start_utf16, text)
    }

    pub fn find_detached_pdf_text(
        self, id: &str, query: &str, start: crate::DetachedPdfFindCursor, limit: usize, target: Option<&str>,
    ) -> Result<crate::DetachedPdfFindPage> {
        self.open(OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .find_detached_pdf_text(id, query, start, limit, target)
    }

    pub fn detached_pdf_translation(
        self, id: &str, page: u16, sentence_id: &str, target: &str, cached_only: bool, background: bool,
    ) -> Result<Option<crate::DetachedPdfTranslation>> {
        let flags = if cached_only { OpenFlags::SQLITE_OPEN_READ_ONLY } else { OpenFlags::SQLITE_OPEN_READ_WRITE };
        self.open(flags)?.detached_pdf_translation(id, page, sentence_id, target, cached_only, background)
    }

    pub fn detached_pdf_passage(
        self, id: &str, page: u16, sentence_id: &str, target: &str, cached_only: bool, background: bool,
    ) -> Result<crate::DetachedPdfPassage> {
        let flags = if cached_only { OpenFlags::SQLITE_OPEN_READ_ONLY } else { OpenFlags::SQLITE_OPEN_READ_WRITE };
        self.open(flags)?.detached_pdf_passage(id, page, sentence_id, target, cached_only, background)
    }

    pub fn advance_background_translation(
        self, id: &str, revision: &str, target: &str,
    ) -> Result<crate::translation_queue::BackgroundTranslationProgress> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.advance_background_translation(id, revision, target)
    }

    pub fn warm_translation_step(
        self, id: &str, revision: &str, target: &str, start: usize,
    ) -> Result<crate::translation_queue::TranslationWarmStep> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.warm_translation_step(id, revision, target, start)
    }

    pub fn pdf_page_translation(
        self, id: &str, page: u16, target: &str, cached_only: bool,
    ) -> Result<Vec<crate::pdf_detached_pages::PdfPageTranslationRow>> {
        self.pdf_page_translation_with_priority(id, page, target, cached_only, false)
    }

    pub fn pdf_page_translation_with_priority(
        self, id: &str, page: u16, target: &str, cached_only: bool, background: bool,
    ) -> Result<Vec<crate::pdf_detached_pages::PdfPageTranslationRow>> {
        let flags = if cached_only { OpenFlags::SQLITE_OPEN_READ_ONLY } else { OpenFlags::SQLITE_OPEN_READ_WRITE };
        self.open(flags)?.pdf_page_translation_with_priority(id, page, target, cached_only, background)
    }

    pub fn translate_detached_pdf_sentence(
        self, id: &str, page: u16, sentence_id: &str, target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .translate_detached_pdf_sentence(id, page, sentence_id, target)
    }

    pub fn translate_paragraph(
        self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
        background: bool,
    ) -> Result<ParagraphTranslation> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?
            .translate_paragraph_with_priority(document_id, sentence_id, target, background)
    }

    pub fn run_translation_window(
        self, document_id: &str, expected: Option<&crate::translation_queue::TranslationWindowAddress>,
    ) -> Result<Option<ParagraphTranslation>> {
        self.open(OpenFlags::SQLITE_OPEN_READ_WRITE)?.run_translation_window(document_id, expected)
    }

    pub fn cached_translation(
        self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<CachedTranslation> {
        self.open(OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .cached_installed_translation(document_id, sentence_id, target)
    }
    pub fn find_translation_page(
        self,
        document_id: &str,
        target: &str,
        query: &str,
        start: usize,
    ) -> Result<TranslationSearchPage> {
        self.open(OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .find_translation_page(document_id, target, query, start)
    }
}

impl Store {
    pub fn prepare_translation_worker(&self) -> Option<StoreWorker> {
        self.prepare_store_worker()
    }

    pub fn prepare_store_worker(&self) -> Option<StoreWorker> {
        // SQLite resolves the file name and reports an empty name for temporary and in-memory databases.
        let database = Path::new(self.connection.path()?);
        if !database.is_absolute() {
            return None;
        }
        Some(StoreWorker {
            database: database.to_owned(),
            model_directory: self.model_directory.clone(),
            optional_model_directory: self.optional_model_directory.clone(),
            knowledge_directory: self.knowledge_directory.clone(),
        })
    }

    pub fn check_translation_download_space(
        &self,
        source: &str,
        target: &str,
        staging_root: &Path,
        archive_directory: &Path,
    ) -> Result<()> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        let stage = staging_root.join(package.revision);
        fs::create_dir_all(&stage).map_err(model_error)?;
        fs::create_dir_all(archive_directory).map_err(model_error)?;
        fs::create_dir_all(&self.model_directory).map_err(model_error)?;
        let total: u64 = package.sizes.iter().sum();
        let completed: u64 = ASSETS
            .into_iter()
            .zip(package.sizes)
            .map(|(name, expected)| {
                fs::metadata(stage.join(name))
                    .ok()
                    .filter(|info| info.is_file())
                    .map_or(0, |info| info.len().min(expected))
            })
            .sum();
        ensure_capacity([
            (&stage, total - completed),
            (archive_directory, total + PACKAGE_OVERHEAD),
            (&self.model_directory, total + PACKAGE_OVERHEAD),
        ])
    }

    pub fn check_translation_archive_space(&self, directory: &Path, bytes: u64) -> Result<()> {
        if bytes > MAX_ARCHIVE_BYTES {
            return Err("Архів мовного пакета завеликий".into());
        }
        fs::create_dir_all(directory).map_err(model_error)?;
        ensure_capacity([(directory, bytes + PACKAGE_OVERHEAD)])
    }

    pub fn translation_packages(&self) -> Vec<TranslationPackageStatus> {
        let provider = MarianProvider::interactive(&self.model_directory);
        catalog::PACKAGES
            .iter()
            .map(|package| TranslationPackageStatus {
                source: package.source,
                target: package.target,
                repository: package.repository,
                license: package.license,
                files_present: provider.files_present(package),
                has_local_files: provider.path(package).symlink_metadata().is_ok()
                    || provider.removed_path(package).symlink_metadata().is_ok()
                    || provider.previous_path(package).symlink_metadata().is_ok(),
                has_recovery_copy: provider.recovery_paths(package).iter()
                    .any(|path| package_directory(path).unwrap_or(false) && package_files_present(path, package)),
                revision: package.revision,
                validation: provider.validation_result(package),
                assets: ASSETS
                    .into_iter()
                    .zip(package.sizes)
                    .zip(package.hashes)
                    .map(|((name, bytes), sha256)| TranslationAsset {
                        name,
                        bytes,
                        sha256,
                    })
                    .collect(),
            })
            .collect()
    }

    pub fn prepare_translation_package_verification(
        &self,
        source: &str,
        target: &str,
    ) -> Result<TranslationPackageVerification> {
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає пакета.")?;
        Ok(TranslationPackageVerification {
            directory: self.model_directory.clone(),
            package,
        })
    }

    pub fn prepare_translation_package_validation(
        &self,
        source: &str,
        target: &str,
    ) -> Result<TranslationPackageValidation> {
        let samples = probe_evidence::samples(source)
            .filter(|_| probe_evidence::samples(target).is_some())
            .ok_or("Для цієї мовної пари немає еталонних речень.")?;
        let package = catalog::PACKAGES
            .iter()
            .find(|package| package.source == source && package.target == target)
            .ok_or("Для цієї мовної пари немає прямого пакета.")?;
        Ok(TranslationPackageValidation {
            directory: self.model_directory.clone(),
            package,
            samples,
        })
    }

    pub fn translation_package_files(&self) -> TranslationPackageFiles {
        TranslationPackageFiles {
            directory: self.model_directory.clone(),
        }
    }

    pub fn remove_translation_package(&self, source: &str, target: &str) -> Result<bool> {
        self.translation_package_files().remove(source, target)
    }

    pub fn install_translation_package(&self, archive_path: &Path) -> Result<String> {
        self.translation_package_files().install(archive_path)
    }

    pub fn translate_installed_sentence(
        &mut self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        let (document, selected) = self.translation_selection(document_id, sentence_id)?;
        let (text, sentence_ids) = sentence_input(self, &document, selected, target, PageBoundaries::Canonical)?;
        self.translate_machine_sentence(document_id, &document.language, target, text, sentence_ids)
    }

    pub fn translate_detached_pdf_sentence(
        &mut self, id: &str, page: u16, sentence_id: &str, target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        let (language, input) = self.detached_sentence_input(id, page, sentence_id, target)?;
        self.translate_machine_sentence(id, &language, target, input, vec![sentence_id.to_owned()])
    }

    pub fn translate_installed_paragraph(
        &mut self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<ParagraphTranslation> {
        self.translate_paragraph_with_priority(document_id, sentence_id, target, false)
    }

    fn translate_paragraph_with_priority(
        &mut self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
        background: bool,
    ) -> Result<ParagraphTranslation> {
        let (document, selected) = self.translation_selection(document_id, sentence_id)?;
        self.translate_document_paragraph(&document, selected, target, background, PageBoundaries::Canonical)
    }

    pub(crate) fn translate_document_paragraph(
        &mut self,
        document: &crate::Document,
        selected: usize,
        target: &str,
        background: bool,
        boundaries: PageBoundaries,
    ) -> Result<ParagraphTranslation> {
        let directory = self.model_directory.clone();
        let translated = translate_paragraph(
            self,
            &mut MarianProvider {
                directory: &directory,
                background,
            },
            document, selected, target, boundaries,
        )?;
        if translated.units.iter().any(|unit| !unit.reviewed && unit.translation.is_some()) {
            return Ok(translated);
        }
        let routes = self.translation_cache_routes(&document.language, target)?;
        Ok(routes.cached_window(self, document, selected, target, boundaries)?.paragraph)
    }

    pub fn cached_installed_translation(
        &self,
        document_id: &str,
        sentence_id: &str,
        target: &str,
    ) -> Result<CachedTranslation> {
        let (document, selected) = self.translation_selection(document_id, sentence_id)?;
        let installed = MarianProvider::interactive(&self.model_directory).installed_models()?;
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        cached_translation(self, &installed, &catalog, &document, selected, target)
    }

    pub(crate) fn translate_document_page(
        &mut self, document: &crate::Document, physical: std::ops::Range<usize>, target: &str,
        boundaries: PageBoundaries, background: bool,
    ) -> Result<Vec<ParagraphUnit>> {
        let directory = self.model_directory.clone();
        translate_pdf_page(self, &mut MarianProvider { directory: &directory, background }, document, physical, target, boundaries)
    }

    pub fn find_translation_page(
        &self,
        document_id: &str,
        target: &str,
        query: &str,
        start: usize,
    ) -> Result<TranslationSearchPage> {
        let document = self.translation_document(document_id)?;
        let installed = MarianProvider::interactive(&self.model_directory).installed_models()?;
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        search_cached_translation(self, &installed, &catalog, &document, target, query, start)
    }

    pub(crate) fn translation_cache_routes(
        &self, source: &str, target: &str,
    ) -> Result<crate::translation::CacheRoutes> {
        let installed = MarianProvider::interactive(&self.model_directory).installed_models()?;
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        crate::translation::CacheRoutes::resolve(&installed, &catalog, source, target)
    }

    fn translation_selection(
        &self,
        document_id: &str,
        sentence_id: &str,
    ) -> Result<(crate::Document, usize)> {
        let document = self.translation_document(document_id)?;
        let selected = document
            .sentences
            .iter()
            .position(|sentence| sentence.id == sentence_id)
            .ok_or("Речення для перекладу не знайдено")?;
        Ok((document, selected))
    }

    pub(crate) fn translation_document(&self, document_id: &str) -> Result<crate::Document> {
        let body: String = self
            .connection
            .query_row(
                "SELECT body FROM documents WHERE id=?1",
                [document_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let mut document = serde_json::from_str(&body).map_err(|error| error.to_string())?;
        crate::document_layout::reflow_ocr_paragraphs(&mut document);
        Ok(document)
    }

    pub fn translation_readiness(
        &self,
        source: &str,
        target: &str,
    ) -> Result<TranslationReadiness> {
        let provider = MarianProvider::interactive(&self.model_directory);
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        route_readiness(&catalog, &provider.installed_models()?, source, target)
    }

    pub(crate) fn background_translation_route(
        &self,
        source: &str,
        target: &str,
    ) -> Result<(String, Option<Vec<crate::translation::TranslationPair>>)> {
        let installed = MarianProvider::interactive(&self.model_directory).installed_models()?;
        if let Some(route) = crate::translation::resolve_route(&installed, source, target)? {
            let identity = serde_json::to_string(&route.without_quality()).map_err(|error| error.to_string())?;
            return Ok((identity, None));
        }
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        let readiness = route_readiness(&catalog, &installed, source, target)?;
        Ok((String::new(), Some(readiness.missing)))
    }

    pub fn translate_dictionary_gloss(
        &mut self, id: &str, source: &str, gloss: &str,
    ) -> Result<Option<AlignedTranslation>> {
        self.translate_dictionary_gloss_to(id, source, gloss, "uk")
    }

    pub fn translate_dictionary_gloss_to(
        &mut self, id: &str, source: &str, gloss: &str, target: &str,
    ) -> Result<Option<AlignedTranslation>> {
        crate::valid_language(target)?;
        crate::valid_language(source)?;
        let gloss = gloss.trim();
        if gloss.is_empty() || gloss.len() > 4096 || gloss.contains('\0') {
            return Err("Некоректний розмір словникового визначення.".into());
        }
        if !self.connection.query_row("SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1)",
            [id], |row| row.get::<_, bool>(0)).map_err(|error| error.to_string())?
        {
            return Err("Документ не знайдено.".into());
        }
        self.translate_machine_sentence(id, source, target, gloss.to_owned(), Vec::new())
    }

    pub fn translate_dictionary_glosses(
        &mut self, id: &str, source: &str, target: &str, glosses: &[String],
    ) -> Result<Vec<Option<String>>> {
        crate::valid_language(source)?;
        crate::valid_language(target)?;
        if !(1..=12).contains(&glosses.len())
            || glosses.iter().any(|gloss| gloss.trim().is_empty() || gloss.len() > 4096 || gloss.contains('\0'))
        {
            return Err("Некоректний обсяг словникових визначень.".into());
        }
        if !self.connection.query_row("SELECT EXISTS(SELECT 1 FROM documents WHERE id=?1)",
            [id], |row| row.get::<_, bool>(0)).map_err(|error| error.to_string())?
        {
            return Err("Документ не знайдено.".into());
        }
        let input: Vec<_> = glosses.iter().map(|gloss| gloss.trim().to_owned()).collect();
        if source == target {
            return Ok(input.into_iter().map(Some).collect());
        }
        let directory = self.model_directory.clone();
        if let Some(result) =
            self.translate_units(&mut MarianProvider::interactive(&directory), source, target, &input)?
        {
            self.record_translation_cache_use(id, &result.route, &input)?;
            return Ok(result.segments.into_iter().map(Some).collect());
        }
        let routes = self.translation_cache_routes(source, target)?;
        input.iter().map(|gloss| {
            let Some(result) = routes.cached_sentence(self, gloss, target)? else { return Ok(None); };
            self.record_translation_cache_use(id, &result.route, std::slice::from_ref(gloss))?;
            Ok(result.segments.into_iter().next())
        }).collect()
    }

    pub fn translate_selected_text(
        &mut self, id: &str, target: &str, selection: &TranslationSelection,
    ) -> Result<Option<SelectedTranslation>> {
        use crate::pdf_detached_pages::word_break;
        fn resolve<'a>(sentences: &'a [crate::Sentence], revision: Option<&str>, selection: &TranslationSelection) -> Result<&'a crate::Sentence> {
            if revision != Some(selection.revision_id.as_str()) {
                return Err("Текст змінився. Виділіть фрагмент знову.".into());
            }
            let (sentence, _) = crate::knowledge::resolve_text_selection(
                sentences, selection.start_utf16, &selection.text,
            )?;
            Ok(sentence)
        }
        let cleaned = |document: &crate::Document, sentence: &crate::Sentence,
                       projected: word_break::ProjectedSelection<'_>| {
            let projected_text = projected.text.into_owned();
            let projected_context = projected.context.into_owned();
            let mask = crate::document_layout::mask_pdf_note_references;
            if projected_context == sentence.text {
                let source = mask(document, &projected_text, sentence.start_utf8 + projected.start_utf8)
                    .map(|text| text.trim().to_owned()).unwrap_or(projected_text);
                let context = mask(document, &projected_context, sentence.start_utf8).unwrap_or(projected_context);
                (source, context)
            } else {
                (projected_text, projected_context)
            }
        };
        let (language, source, context) = if let Some(page) = selection.page {
            let cached = self.detached_pdf_sentences(id, page)?
                .ok_or("Текст сторінки PDF недоступний.")?;
            let sentence = resolve(&cached.sentences, Some(cached.revision.as_str()), selection)?;
            let projected = word_break::project_selection(
                self, (id, page), &cached.sentences, sentence, selection.start_utf16, &selection.text,
            )?;
            let layout = crate::document_layout::detached_pdf_document(
                cached.text, &cached.language, self.detached_pdf_ocr(id, page)?.map(|ocr| ocr.lines),
            );
            let (source, context) = cleaned(&layout, sentence, projected);
            (cached.language, source, context)
        } else {
            let document = self.translation_document(id)?;
            let sentence = resolve(&document.sentences, document.current_revision_id.as_deref(), selection)?;
            let projected = word_break::project_canonical_selection(
                self, &document, sentence, selection.start_utf16, &selection.text,
            )?;
            let (source, context) = cleaned(&document, sentence, projected);
            (document.language, source, context)
        };
        if source.is_empty() {
            return Ok(None);
        }
        if language == "fr" && target == "uk" {
            if let Some(number) = crate::french_cardinals::compound(&source) {
                let text = crate::tts::uk_numbers::cardinal(number).replace('\'', "’");
                return Ok(Some(SelectedTranslation::Cardinal { text, source_text: source }));
            }
        }
        let context = (context != source && context.len() <= 768).then_some(context.as_ref());
        let input = [crate::translation::normalize_cardinals(&language, target, source.clone())];
        let directory = self.model_directory.clone();
        let result = self.translate_aligned(
            &mut MarianProvider::interactive(&directory),
            TranslationRequest {
                source: &language,
                target,
                segments: &input,
                context,
                settings: TranslationSettings::default(),
            },
        )?;
        let result = match result {
            Some(translation) => Some(translation),
            None => self.translation_cache_routes(&language, target)?
                .cached_contextual_selection(self, &input[0], context, target)?,
        };
        Ok(result.map(|translation| SelectedTranslation::Model { translation, source_text: source }))
    }

    pub fn translation_routes(&self) -> Result<Vec<TranslationRouteStatus>> {
        let provider = MarianProvider::interactive(&self.model_directory);
        let catalog: Vec<_> = catalog::PACKAGES.iter().map(Package::edge).collect();
        let installed = provider.installed_models()?;
        let languages: BTreeSet<_> = catalog
            .iter()
            .flat_map(|edge| [edge.source.as_str(), edge.target.as_str()])
            .collect();
        let mut routes = Vec::new();
        for &source in &languages {
            for &target in &languages {
                if source == target {
                    continue;
                }
                let readiness = route_readiness(&catalog, &installed, source, target)?;
                if readiness.ready || !readiness.missing.is_empty() {
                    routes.push(TranslationRouteStatus {
                        source: source.to_owned(),
                        target: target.to_owned(),
                        ready: readiness.ready,
                        pivot_language: readiness.pivot_language,
                    });
                }
            }
        }
        Ok(routes)
    }

    fn translate_machine_sentence(
        &mut self,
        document_id: &str,
        source: &str,
        target: &str,
        text: String,
        sentence_ids: Vec<String>,
    ) -> Result<Option<AlignedTranslation>> {
        if text.trim().is_empty() { return Ok(None); }
        let directory = self.model_directory.clone();
        let translated = self.translate_aligned(
            &mut MarianProvider::interactive(&directory),
            TranslationRequest {
                source,
                target,
                segments: std::slice::from_ref(&text),
                context: None,
                settings: TranslationSettings::default(),
            },
        )?;
        let result = match translated {
            Some(translated) => Some(translated),
            None => self.translation_cache_routes(source, target)?.cached_sentence(self, &text, target)?,
        };
        result.map(|mut translation| {
            self.record_translation_cache_use(document_id, &translation.route, std::slice::from_ref(&text))?;
            translation.attach_source_sentences(sentence_ids);
            Ok(translation)
        }).transpose()
    }
}

impl<'a> MarianProvider<'a> {
    fn interactive(directory: &'a Path) -> Self {
        Self {
            directory,
            background: false,
        }
    }

    fn active_package_verified(&self, package: &Package) -> Result<bool> {
        let _lock = self.lock(package, false)?;
        let active = self.path(package);
        Ok(package_directory(&active)?
            && verify_files(&active, package, &ASSETS, &|| Ok(())).is_ok())
    }

    // Install operations hold the root staging lock. They leave the current
    // package available while proving the candidate can run offline.
    fn activate_staged(&self, package: &Package, stage: &Path) -> Result<()> {
        {
            let job = RESOURCES.optional()?;
            let checkpoint = || {
                if job.should_yield() {
                    return Err("Перевірку нового пакета перервано активним запитом. Спробуйте ще раз.".into());
                }
                Ok(())
            };
            let [sample, ..] = probe_evidence::samples(package.source)
                .ok_or("Для цього пакета немає контрольного речення.")?;
            let result = Self::translate_package(
                stage, package, &[sample.to_owned()], TranslationSettings { beam_size: 1 }, checkpoint,
            ).map_err(|error| format!("Новий пакет не пройшов пробний переклад. {error}"))?;
            if !matches!(result.as_slice(), [output] if output.chars().any(char::is_alphanumeric)) {
                return Err("Новий пакет не повернув текст пробного перекладу.".into());
            }
        }
        let _lock = self.lock(package, true)?;
        activate_verified_package(
            stage, &self.path(package), &self.previous_path(package), &self.removed_path(package),
        )
    }

    fn files_present(&self, model: &Package) -> bool {
        package_files_present(&self.read_path(model), model)
    }

    fn recovery_paths(&self, model: &Package) -> [PathBuf; 2] {
        // The second retained slot holds the latest replacement when .previous already exists.
        [self.removed_path(model), self.previous_path(model)]
    }

    fn validation_result(&self, model: &Package) -> Option<TranslationPackageValidationResult> {
        if !self.read_path(model).is_dir() {
            return None;
        }
        let _lock = self.lock(model, false).ok()?;
        probe_evidence::read(&self.read_path(model), model)
    }

    fn path(&self, model: &Package) -> PathBuf {
        self.directory.join(model.repository)
    }

    fn removed_path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!(".removed-{}", model.repository))
    }

    fn previous_path(&self, model: &Package) -> PathBuf {
        self.directory
            .join(format!(".previous-{}", model.repository))
    }

    fn read_path(&self, model: &Package) -> PathBuf {
        let active = self.path(model);
        if active.exists() {
            active
        } else {
            self.previous_path(model)
        }
    }

    fn lock(&self, model: &Package, exclusive: bool) -> Result<File> {
        let path = self.directory.join(format!(".{}.lock", model.repository));
        lock_package(&path, exclusive)
    }
}

impl TranslationProvider for MarianProvider<'_> {
    fn installed_models(&self) -> Result<Vec<ModelEdge>> {
        Ok(catalog::PACKAGES
            .iter()
            .filter(|model| self.files_present(model))
            .map(|model| {
                let mut edge = model.edge();
                edge.quality = probe_evidence::read(&self.read_path(model), model)
                    .and_then(|validation| validation.quality);
                edge
            })
            .collect())
    }

    fn allow_alternative_routes(&self) -> bool {
        !self.background
    }

    fn translate(
        &mut self,
        edge: &ModelEdge,
        segments: &[String],
        _context: Option<&str>,
        settings: TranslationSettings,
    ) -> Result<Vec<String>> {
        if !(1..=8).contains(&settings.beam_size) {
            return Err("Некоректні параметри декодування перекладу".into());
        }
        let package = catalog::PACKAGES
            .iter()
            .find(|package| {
                package.revision == edge.revision
                    && package.source == edge.source
                    && package.target == edge.target
            })
            .ok_or("Невідомий мовний пакет")?;
        let _interactive = (!self.background).then(|| RESOURCES.interactive());
        // A background job can wait for package management without owning the compute permit.
        let _lock = self.lock(package, false)?;
        let optional = self.background.then(|| RESOURCES.optional()).transpose()?;
        let checkpoint = || {
            if optional.as_ref().is_some_and(|job| job.should_yield()) {
                return Err("Фоновий переклад поступився активному запиту.".into());
            }
            Ok(())
        };
        Self::translate_package(&self.read_path(package), package, segments, settings, checkpoint)
    }
}

impl MarianProvider<'_> {
    fn translate_package(
        directory: &Path,
        package: &Package,
        segments: &[String],
        settings: TranslationSettings,
        checkpoint: impl Fn() -> Result<()>,
    ) -> Result<Vec<String>> {
        verify_files(directory, package, &ASSETS[..4], &checkpoint)?;
        let mut configuration: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("config.json")).map_err(model_error)?)
                .map_err(model_error)?;
        // The verified en-de weights include a shared embedding; that revision
        // predates the explicit config field.
        if package.repository == "en-de" {
            if let Some(fields) = configuration.as_object_mut() {
                fields
                    .entry("share_encoder_decoder_embeddings")
                    .or_insert(true.into());
            }
        }
        let config: Config = serde_json::from_value(configuration).map_err(model_error)?;
        let vocabulary: HashMap<String, u32> =
            serde_json::from_slice(&fs::read(directory.join("vocab.json")).map_err(model_error)?)
                .map_err(model_error)?;
        let output_pieces: HashMap<_, _> = vocabulary
            .iter()
            .map(|(piece, id)| (*id, piece.as_str()))
            .collect();
        let input =
            SentencePieceProcessor::open(directory.join("source.spm")).map_err(model_error)?;
        let output =
            SentencePieceProcessor::open(directory.join("target.spm")).map_err(model_error)?;
        let mut bytes = Vec::new();
        let (_, digest) = copy_and_hash_checked(
            File::open(directory.join("model.safetensors")).map_err(model_error)?,
            &mut bytes,
            asset_limit("model.safetensors"),
            &checkpoint,
        )?;
        if digest != package.hashes[4] {
            return Err("Файл ваг мовного пакета пошкоджений".into());
        }
        let weights = VarBuilder::from_buffered_safetensors(bytes, DType::F32, &Device::Cpu)
            .map_err(model_error)?;
        // SafeTensors stores a tied embedding once. The verified French→English
        // revision names that tensor under the decoder instead of model.shared.
        let weights = if config.share_encoder_decoder_embeddings
            && !weights.contains_tensor("model.shared.weight")
            && weights.contains_tensor("model.decoder.embed_tokens.weight")
        {
            weights.rename_f(|name| match name {
                "model.shared.weight" => "model.decoder.embed_tokens.weight".into(),
                name => name.into(),
            })
        } else {
            weights
        };
        checkpoint()?;
        let mut model = MTModel::new(&config, weights).map_err(model_error)?;
        let unknown = *vocabulary
            .get("<unk>")
            .ok_or("У мовному пакеті немає маркера невідомого слова")?;

        let prefix = package
            .target_prefix
            .map(|tag| {
                vocabulary
                    .get(tag)
                    .copied()
                    .ok_or("Мовний пакет не містить маркера цільової мови")
            })
            .transpose()?;
        let limit = config.max_position_embeddings.min(256);
        let capacity = limit
            .checked_sub(1 + usize::from(prefix.is_some()))
            .filter(|limit| *limit > 0)
            .ok_or("Некоректна довжина входу мовної моделі")?;

        let mut results = Vec::with_capacity(segments.len());
        let mut clock_retry_budget = 2;
        let mut ukrainian_retry_budget = 2;
        for segment in segments {
            checkpoint()?;
            let source_pieces = input
                .encode(normalized_translation_input(package, segment).as_ref())
                .map_err(model_error)?;
            let mut start = 0;
            let mut translated_parts = Vec::new();
            loop {
                checkpoint()?;
                let mut end = source_pieces.len().min(start + capacity);
                if end < source_pieces.len() {
                    end = end.min(start + 160);
                    end = ((start + 1)..=end)
                        .rev()
                        .find(|&index| source_pieces[index].starts_with('▁'))
                        .unwrap_or(end);
                }
                let mut tokens: Vec<_> = prefix
                    .into_iter()
                    .chain(
                        source_pieces[start..end]
                            .iter()
                            .map(|piece| vocabulary.get(piece).copied().unwrap_or(unknown)),
                    )
                    .collect();
                tokens.push(config.eos_token_id);
                model.reset_kv_cache();
                let encoder_input = Tensor::new(tokens.as_slice(), &Device::Cpu)
                    .and_then(|x| x.unsqueeze(0))
                    .map_err(model_error)?;
                let encoded = model
                    .encoder()
                    .forward(&encoder_input, 0)
                    .map_err(model_error)?;
                let decode_text = |tokens: &[u32]| -> Result<String> {
                    let pieces = tokens
                        .iter()
                        .map(|token| {
                            output_pieces
                                .get(token)
                                .copied()
                                .ok_or("Невідомий токен перекладу")
                        })
                        .collect::<std::result::Result<Vec<_>, _>>()?;
                    output
                        .decode(&pieces)
                        .map(|text| text.trim().to_owned())
                        .map_err(model_error)
                };
                let mut decoded = decoder::candidates(
                    &model,
                    &encoded,
                    &config,
                    settings.beam_size,
                    limit,
                    &checkpoint,
                )?
                .into_iter();
                let best = decoded
                    .next()
                    .ok_or("Модель не завершила переклад речення")?;
                let mut translated = decode_text(&best)?;
                let mut reference = |text: &str, width| -> Result<String> {
                    checkpoint()?;
                    let pieces = input
                        .encode(normalized_translation_input(package, text).as_ref())
                        .map_err(model_error)?;
                    if pieces.len() > capacity {
                        return Err("Речення завелике для перевірки контексту".into());
                    }
                    let mut ids: Vec<_> = prefix
                        .into_iter()
                        .chain(
                            pieces
                                .iter()
                                .map(|piece| vocabulary.get(piece).copied().unwrap_or(unknown)),
                        )
                        .collect();
                    ids.push(config.eos_token_id);
                    model.reset_kv_cache();
                    let tensor = Tensor::new(ids.as_slice(), &Device::Cpu)
                        .and_then(|value| value.unsqueeze(0))
                        .map_err(model_error)?;
                    let encoded = model.encoder().forward(&tensor, 0).map_err(model_error)?;
                    decode_text(&decoder::decode(
                        &model,
                        &encoded,
                        &config,
                        width,
                        limit,
                        &checkpoint,
                    )?)
                };
                let units: Vec<_> = contextual_sentences(segment).collect();
                let mut alternatives = Vec::new();
                if start == 0
                    && end == source_pieces.len()
                    && supports_contextual_recovery(segment, units.len())
                    && decoded.len() > 0
                {
                    alternatives.push(translated.clone());
                    for text in decoded
                        .by_ref()
                        .filter_map(|tokens| decode_text(&tokens).ok())
                    {
                        if !alternatives.contains(&text) {
                            alternatives.push(text);
                        }
                    }
                    let references = units
                        .iter()
                        .map(|text| {
                            let greedy = reference(text, 1)?;
                            // A greedy paraphrase absent from every candidate cannot arbitrate.
                            // One bounded width-2 retry may recover discriminating evidence.
                            if rerank::supported_reference(&alternatives, &greedy) {
                                Ok(greedy)
                            } else {
                                reference(text, 2).or(Ok(greedy))
                            }
                        })
                        .collect::<Result<Vec<_>>>();
                    if let Ok(references) = references {
                        let chosen = rerank::choose(&alternatives, &references);
                        let recovered = rerank::recover_omission(&alternatives, chosen, &references, || {
                            units.iter().zip(&references).all(|(source, target)| {
                                !source.eq_ignore_ascii_case(target)
                                    && !crate::language_detection::detect(target)
                                        .is_some_and(|language| language != package.target)
                                    && (package.target != "uk"
                                        || !crate::marian_target_uk::clearly_russian(target))
                            })
                        });
                        translated = recovered.unwrap_or_else(|| alternatives[chosen].clone());
                    }
                }
                // Retry only the observed clock-as-year artifact, using the same loaded model.
                // The baseline remains authoritative unless the retry proves the source clock.
                if package.source == "en"
                    && package.target == "uk"
                    && start == 0
                    && end == source_pieces.len()
                    && segment.len() <= 768
                    && clock_retry_budget > 0
                {
                    if let Some(retry) = time::retry_input(segment, &translated) {
                        clock_retry_budget -= 1;
                        if let Ok(candidate) = reference(&retry.input, settings.beam_size) {
                            if retry.accepts(&candidate) {
                                translated = candidate;
                            }
                        }
                    }
                }
                if package.source == "uk"
                    && package.target == "fr"
                    && start == 0
                    && end == source_pieces.len()
                    && segment.len() <= 768
                {
                    if (2..=3).contains(&units.len()) {
                        if let Some(text) = crate::marian_deadline::restore_group(&units, &translated) {
                            translated = text;
                        }
                    } else if let [source] = units.as_slice() {
                        if let Some(deadline) = crate::marian_deadline::find(source) {
                            if !deadline.accepts(&translated) {
                                let alternative = decoded
                                    .by_ref()
                                    .filter_map(|tokens| decode_text(&tokens).ok())
                                    .find(|text| deadline.preserves_clause(&translated, text));
                                if let Some(text) =
                                    alternative.or_else(|| deadline.restore_comparison(&translated))
                                {
                                    translated = text;
                                }
                            }
                        }
                    }
                }
                if package.source == "fr" && package.target == "uk" &&
                    start == 0 && end == source_pieces.len() && units.len() == 1
                {
                    if let Some(text) = crate::marian_target_uk::repair_french_affirmative(segment, &translated) {
                        translated = text;
                    }
                }
                if package.target == "uk" && crate::marian_target_uk::clearly_russian(&translated) {
                    let candidate = alternatives
                        .into_iter()
                        .chain(decoded.filter_map(|tokens| decode_text(&tokens).ok()))
                        .find(|text| crate::marian_target_uk::clearly_ukrainian(text));
                    translated = match candidate {
                        Some(text) => text,
                        None if ukrainian_retry_budget > 0
                            && start == 0
                            && end == source_pieces.len()
                            && segment.len() <= 768
                            && units.len() == 1 =>
                        {
                            ukrainian_retry_budget -= 1;
                            let sentence = segment.trim_end();
                            let retry = match sentence.chars().last() {
                                Some(mark @ ('.' | '!' | '?')) => {
                                    let source =
                                        sentence[..sentence.len() - mark.len_utf8()].trim_end();
                                    reference(source, settings.beam_size)
                                        .ok()
                                        .filter(|text| {
                                            crate::marian_target_uk::clearly_ukrainian(text)
                                        })
                                        .map(|text| {
                                            format!(
                                                "{}{mark}",
                                                text.trim_end_matches(['.', '!', '?'])
                                            )
                                        })
                                }
                                _ => reference(segment, 1).ok().filter(|text| {
                                    crate::marian_target_uk::clearly_ukrainian(text)
                                }),
                            };
                            retry
                                .ok_or("Модель повернула російський текст замість українського.")?
                        }
                        None => {
                            return Err(
                                "Модель повернула російський текст замість українського.".into()
                            )
                        }
                    };
                }
                // Reranking/retry tolerate inference errors. A yield must not cache their fallback.
                checkpoint()?;
                translated_parts.push(translated);
                if end == source_pieces.len() {
                    break;
                }
                start = end;
            }
            let mut translated = translated_parts.join(" ");
            if package.source == "fr" && package.target == "uk" {
                crate::translation_dates::preserve_french_dates(segment, &mut translated);
            }
            results.push(translated);
        }
        Ok(results)
    }
}

fn normalized_translation_input<'a>(package: &Package, text: &'a str) -> Cow<'a, str> {
    let input = normalized_input(package.source, text);
    if package.source == "fr" && package.target == "uk" {
        // The direct Marian model can read "pas" as a path here. Make the gait explicit.
        if let Some(clarified) = crate::marian_target_uk::clarify_french_manner_of_walking(&input) {
            return Cow::Owned(clarified);
        }
    }
    input
}

fn normalized_input<'a>(source: &str, text: &'a str) -> Cow<'a, str> {
    if source == "fr" && text.contains('’') {
        Cow::Owned(text.replace('’', "'"))
    } else {
        Cow::Borrowed(text)
    }
}

fn package_files_present(path: &Path, package: &Package) -> bool {
    ASSETS.iter().zip(package.sizes).all(|(asset, expected)| {
        fs::symlink_metadata(path.join(asset))
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected)
    })
}

fn asset_limit(asset: &str) -> u64 {
    if asset == "model.safetensors" {
        400_000_000
    } else {
        8_000_000
    }
}

fn verify_files(
    directory: &Path,
    package: &Package,
    assets: &[&str],
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<()> {
    for (index, asset) in assets.iter().take(ASSETS.len()).enumerate() {
        copy_verified_asset(directory, package, index, asset, io::sink(), checkpoint)?;
    }
    Ok(())
}

fn copy_verified_asset(
    directory: &Path,
    package: &Package,
    index: usize,
    asset: &str,
    output: impl io::Write,
    checkpoint: &impl Fn() -> Result<()>,
) -> Result<()> {
    let path = directory.join(asset);
    let expected_size = package.sizes[index];
    let read_error = |error| format!("Не вдалося прочитати файл мовного пакета {asset}: {error}");
    let metadata = fs::symlink_metadata(&path).map_err(read_error)?;
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
    }
    let file = File::open(path).map_err(read_error)?;
    let (size, actual) = copy_and_hash_checked(file, output, expected_size, checkpoint)
        .map_err(|error| format!("{asset}: {error}"))?;
    if size != expected_size || actual != package.hashes[index] {
        return Err(format!("Файл мовного пакета пошкоджений: {asset}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn french_apostrophe_normalization_only_changes_model_input() {
        let source = "Marie n’a pas payé les 27 euros à Lyon.";
        assert_eq!(
            normalized_input("fr", source),
            "Marie n'a pas payé les 27 euros à Lyon."
        );
        assert_eq!(source, "Marie n’a pas payé les 27 euros à Lyon.");
        assert!(matches!(normalized_input("uk", source), Cow::Borrowed(_)));
        assert!(matches!(
            normalized_input("fr", "L'heure."),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn failed_activation_restores_active_files_and_keeps_prior_copy() {
        let root = tempfile::tempdir().unwrap();
        let active = root.path().join("active");
        let previous = root.path().join("previous");
        let discarded = root.path().join("discarded");
        let stage = root.path().join("stage");
        fs::create_dir(&active).unwrap();
        fs::write(active.join("model"), "active").unwrap();
        assert!(activate_verified_package(&stage, &active, &previous, &discarded).is_err());
        assert_eq!(fs::read_to_string(active.join("model")).unwrap(), "active");
        assert!(!previous.exists());

        fs::create_dir(&previous).unwrap();
        fs::write(previous.join("model"), "previous").unwrap();
        assert!(activate_verified_package(&stage, &active, &previous, &discarded).is_err());
        assert_eq!(fs::read_to_string(active.join("model")).unwrap(), "active");
        assert_eq!(
            fs::read_to_string(previous.join("model")).unwrap(),
            "previous"
        );

        fs::create_dir(&stage).unwrap();
        fs::write(stage.join("model"), "verified").unwrap();
        activate_verified_package(&stage, &active, &previous, &discarded).unwrap();
        assert_eq!(
            fs::read_to_string(active.join("model")).unwrap(),
            "verified"
        );
        assert_eq!(
            fs::read_to_string(discarded.join("model")).unwrap(),
            "active"
        );
    }

    #[test]
    fn package_removal_is_idempotent_and_clears_prior_interrupted_removal() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("reader.db")).unwrap();
        let root = &store.model_directory;
        let active = root.join("uk-fr");
        let interrupted = root.join(".removed-uk-fr");
        let other = root.join("fr-en");
        for directory in [&active, &interrupted, &other] {
            fs::create_dir_all(directory).unwrap();
            fs::write(directory.join("config.json"), b"partial").unwrap();
        }
        let status = store
            .translation_packages()
            .into_iter()
            .find(|package| package.source == "uk" && package.target == "fr")
            .unwrap();
        assert!(!status.files_present && status.has_local_files);
        assert!(store.remove_translation_package("uk", "fr").unwrap());
        assert!(!active.exists() && !interrupted.exists());
        assert_eq!(fs::read(other.join("config.json")).unwrap(), b"partial");
        assert!(!store.remove_translation_package("uk", "fr").unwrap());
        assert!(store.remove_translation_package("../fr", "en").is_err());
    }
}
