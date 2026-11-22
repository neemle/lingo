use reader_core::{Backup, Vocabulary};
use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const MAX_WIRE: usize = 256 * 1024 * 1024;

#[derive(Deserialize)]
pub struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub command: Command,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Open {
        path: String,
    },
    Snapshot,
    StageIntake {
        input: reader_core::intake::IntakeInput,
        #[serde(rename = "requestId")]
        request_id: Option<String>,
    },
    ListIntake {
        #[serde(default)]
        start: usize,
    },
    IntakeItem {
        id: String,
    },
    IntakePayload {
        id: String,
    },
    CompleteIntake {
        id: String,
        result: reader_core::intake::IntakeImport,
    },
    DiscardIntake {
        id: String,
    },
    ReaderPreferences,
    UpdateReaderPreference {
        change: reader_core::reader_preferences::ReaderPreferenceChange,
    },
    RecentReadingSessions,
    CheckpointReading {
        session: reader_core::reading_history::ReadingSession,
    },
    PrepareUkrainianSpeech {
        text: String,
    },
    DetectLanguage {
        text: String,
    },
    DetectDocumentLanguage {
        name: String,
        bytes: Vec<u8>,
    },
    Import {
        name: String,
        bytes: Vec<u8>,
        language: String,
        #[serde(rename = "sourceUrl")]
        source_url: Option<String>,
    },
    ImportPdfFile {
        name: String,
        path: String,
        language: String,
        #[serde(rename = "pageTexts")]
        page_texts: Vec<String>,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    ImportImage {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    PdfPagesNeedingOcr {
        name: String,
        bytes: Vec<u8>,
        pages: u16,
    },
    PdfReadingPage {
        id: String,
    },
    SetPdfReadingPage {
        id: String,
        page: u16,
    },
    PdfReadWindow {
        id: String,
        center: u16,
    },
    SentenceTokens {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(default)]
        start: usize,
        limit: Option<usize>,
    },
    LookupSentenceToken {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
    },
    AnalyzeLinguistics {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        scope: reader_core::knowledge::AnalysisScope,
    },
    PdfOcrNeededPages {
        id: String,
        center: u16,
    },
    ClaimPdfOcrPage {
        id: String,
    },
    RetryPdfOcrPage {
        id: String,
        page: u16,
        token: String,
        #[serde(default)]
        canceled: bool,
    },
    CachePdfOcrPage {
        id: String,
        page: u16,
        lines: Vec<reader_core::ocr::OcrLine>,
        token: Option<String>,
    },
    PdfOcrJob {
        job: reader_core::ocr_jobs::JobRequest,
    },
    ImportMixedPdf {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
        pages: u16,
    },
    ImportScannedPdf {
        name: String,
        bytes: Vec<u8>,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
        pages: u16,
    },
    CorrectOcrLine {
        id: String,
        index: usize,
        correction: Option<String>,
    },
    SetDocumentLanguage {
        id: String,
        language: String,
    },
    RenameDocument {
        id: String,
        title: String,
    },
    DeleteDocument {
        id: String,
    },
    Position {
        id: String,
        position: usize,
    },
    ReadingCursor {
        id: String,
    },
    SetReadingCursor {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        #[serde(rename = "startUtf16")]
        start_utf16: Option<usize>,
    },
    SpeechResume {
        id: String,
    },
    SetSpeechResume {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "textHash")]
        text_hash: String,
        #[serde(rename = "offsetUtf16")]
        offset_utf16: Option<usize>,
        #[serde(default)]
        model: Option<reader_core::ModelSpeechResume>,
    },
    SetSpeechText {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        text: String,
    },
    SetBookmark {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        saved: bool,
    },
    SetSentenceNote {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        note: String,
    },
    SetReviewedTranslation {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        text: String,
    },
    SemanticVocabulary {
        query: String,
        #[serde(rename = "documentId")]
        document_id: Option<String>,
    },
    ExplainWord {
        model: String,
        document: String,
        sentence: String,
        word: String,
    },
    OptionalModels,
    ModelTransfer {
        package: reader_core::model_transfer::ModelPackage,
        action: reader_core::model_transfer::TransferAction,
    },
    CheckOptionalModelSpace {
        id: String,
        #[serde(rename = "stagingRoot")]
        staging_root: String,
    },
    InstallOptionalModel {
        id: String,
        #[serde(rename = "stagingRoot")]
        staging_root: String,
    },
    ExportOptionalModelArchive {
        id: String,
        path: String,
    },
    InstallOptionalModelArchive {
        id: String,
        path: String,
    },
    RollbackOptionalModel {
        id: String,
    },
    VerifyOptionalModel {
        id: String,
    },
    RemoveOptionalModel {
        id: String,
    },
    TranslationPackages,
    TranslationReadiness {
        source: String,
        target: String,
    },
    KnowledgePackages,
    LexicalPackages,
    InstallLexicalPackage {
        language: String,
        path: String,
    },
    DictionarySources,
    DictionaryCollection {
        #[serde(rename = "documentId")]
        document_id: Option<String>,
    },
    ExportDictionary {
        #[serde(rename = "documentId")]
        document_id: Option<String>,
    },
    ImportDictionary {
        archive: reader_core::dictionary_transfer::DictionaryArchive,
    },
    InstallKnowledgePackage {
        language: String,
        path: String,
    },
    LookupWord {
        language: String,
        word: String,
        context: Option<reader_core::knowledge::WordContext>,
    },
    SuggestOcr {
        language: String,
        text: String,
    },
    InstallTranslationPackage {
        path: String,
    },
    ExportTranslationPackage {
        source: String,
        target: String,
        path: String,
    },
    VerifyTranslationPackage {
        source: String,
        target: String,
    },
    ValidateTranslationPackage {
        source: String,
        target: String,
    },
    RemoveTranslationPackage {
        source: String,
        target: String,
    },
    CheckTranslationDownloadSpace {
        source: String,
        target: String,
        #[serde(rename = "stagingRoot")]
        staging_root: String,
        #[serde(rename = "archiveDirectory")]
        archive_directory: String,
    },
    CheckTranslationArchiveSpace {
        directory: String,
        bytes: u64,
    },
    SynthesizeSpeech {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        engine: Option<String>,
        speaker: u32,
        rate: f32,
    },
    PrefetchSpeech {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        engine: Option<String>,
        speaker: u32,
        rate: f32,
    },
    TranslateParagraph {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(default)]
        background: bool,
    },
    ScheduleTranslationWindow {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        target: String,
    },
    PendingTranslationWindow {
        id: String,
    },
    RunTranslationWindow {
        id: String,
    },
    CancelTranslationWindow {
        id: String,
    },
    FindTranslationPage {
        id: String,
        target: String,
        query: String,
        start: usize,
    },
    CachedTranslation {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
    },
    TranslateSentence {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
    },
    SaveWord {
        entry: Vocabulary,
    },
    DeleteVocabularyCard {
        id: String,
    },
    SetVocabularyVariants {
        id: String,
        translation: Option<String>,
        #[serde(rename = "acceptedTranslations")]
        accepted_translations: Vec<String>,
    },
    VocabularyReviewHistory {
        id: String,
    },
    Review {
        id: String,
        direction: String,
        grade: u8,
    },
    Practice,
    NextPracticeQuestion {
        revision: u64,
    },
    AnswerPracticeQuestion {
        revision: u64,
        answer: Option<String>,
        #[serde(rename = "elapsedMs")]
        elapsed_ms: Option<u32>,
    },
    SetPractice {
        progress: reader_core::practice::PracticeProgress,
    },
    GradePractice {
        revision: u64,
        grade: u8,
    },
    Original {
        id: String,
    },
    OriginalFile {
        id: String,
    },
    Export,
    ExportBackupFile {
        path: String,
    },
    InspectBackupFile {
        path: String,
    },
    RestoreBackupFile {
        path: String,
    },
    Restore {
        backup: Backup,
    },
}

#[derive(Debug, Serialize)]
pub struct Error {
    pub version: u32,
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            version: VERSION,
            code,
            message: message.into(),
            retryable: false,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
