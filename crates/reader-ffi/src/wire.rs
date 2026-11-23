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
    StorageUsage,
    ClearStorage {
        scope: reader_core::storage_management::StorageScope,
    },
    AdvanceBackgroundTranslation {
        id: String,
        revision: String,
        target: String,
    },
    WarmTranslationStep {
        id: String,
        revision: String,
        target: String,
        start: usize,
    },
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
    FindIntake {
        id: String,
    },
    IntakePayload {
        id: String,
    },
    IntakeArticle {
        id: String,
    },
    SaveIntakeArticle {
        id: String,
        draft: reader_core::intake::ArticleDraft,
    },
    IntakePhotoOcrLanguage {
        id: String,
    },
    SaveIntakePhotoOcr {
        id: String,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    IntakePdfTextSample {
        id: String,
    },
    SaveIntakePdfText {
        id: String,
        draft: reader_core::intake::PdfTextDraft,
    },
    CompleteIntake {
        id: String,
        result: reader_core::intake::IntakeImport,
    },
    DiscardIntake {
        id: String,
    },
    ReaderPreferences,
    MergeReaderPreferences {
        preferences: reader_core::reader_preferences::ReaderPreferences,
    },
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
        #[serde(default, rename = "emptyPages")]
        empty_pages: Vec<u16>,
        #[serde(rename = "sourceHash")]
        source_hash: Option<String>,
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
    OpenPdfReadingPage {
        id: String,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        page: u16,
    },
    PdfReadWindow {
        id: String,
        center: u16,
    },
    UnreadPdfTextPages {
        id: String,
    },
    DetachedPdfSentence {
        id: String,
        page: u16,
        position: Option<usize>,
        #[serde(rename = "sourceOffset")]
        source_offset: Option<usize>,
        #[serde(rename = "sentenceId")]
        sentence_id: Option<String>,
    },
    FindDetachedPdfText {
        id: String,
        query: String,
        target: Option<String>,
        start: reader_core::DetachedPdfFindCursor,
        limit: usize,
    },
    SetDetachedPdfSpeechResume {
        id: String,
        page: u16,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        resume: Option<reader_core::SpeechResume>,
    },
    DetachedPdfOcr { id: String, page: u16 },
    FocusDetachedPdfOcrWord {
        id: String,
        page: u16,
        revision: String,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
        word: String,
    },
    CorrectDetachedPdfOcr {
        id: String,
        page: u16,
        revision: String,
        index: usize,
        correction: Option<String>,
    },
    DetachedPdfAnnotations { id: String, page: u16 },
    UpdateDetachedPdfAnnotation {
        id: String,
        page: u16,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        change: reader_core::DetachedPdfAnnotationChange,
    },
    DetachedPdfTranslation {
        id: String,
        page: u16,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(rename = "cachedOnly")]
        cached_only: bool,
        #[serde(default)]
        background: bool,
    },
    DetachedPdfPassage {
        id: String,
        page: u16,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(rename = "cachedOnly")]
        cached_only: bool,
        #[serde(default)]
        background: bool,
    },
    TranslateDetachedPdfSentence {
        id: String,
        page: u16,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
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
    AnalyzeDetachedPdf {
        id: String,
        page: u16,
        request: reader_core::knowledge::DetachedPageAnalysis,
    },
    PrefetchLinguistics {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        step: i8,
        page: Option<u16>,
    },
    PdfOcrNeededPages {
        id: String,
        center: u16,
    },
    ClaimPdfOcrPage {
        id: String,
        center: Option<u16>,
    },
    CacheBackgroundPdfTextLayer {
        id: String,
        revision: String,
        page: u16,
        text: String,
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
        #[serde(rename = "textLayer")]
        text_layer: Option<String>,
    },
    ReplacePdfTextLayer {
        id: String,
        page: u16,
        #[serde(rename = "revisionId")]
        revision_id: String,
        language: String,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    ReconcilePdfOcrPage {
        id: String,
        page: u16,
        #[serde(rename = "revisionId")]
        revision_id: String,
        language: String,
        reference: String,
        #[serde(default)]
        detached: bool,
    },
    ReconcilePhotoOcr {
        id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
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
    AnchorReadingSelection {
        id: String,
        #[serde(rename = "revisionId")]
        revision_id: String,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
        #[serde(rename = "lengthUtf16")]
        length_utf16: usize,
    },
    SpeechResume {
        id: String,
    },
    SpeechNarration {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(default, rename = "pronunciationOnly")]
        pronunciation_only: bool,
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
    TranslateWithModel {
        id: String,
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(default)]
        regenerate: bool,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelModelTranslation {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    ReviewSentence {
        model: String,
        document: String,
        page: Option<u16>,
        sentence: Option<String>,
        focus: Option<reader_core::qwen::ReviewFocus>,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
    },
    ExplainWord {
        model: String,
        document: String,
        sentence: String,
        word: String,
        #[serde(rename = "startUtf16")]
        start_utf16: Option<usize>,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
    },
    ExplainDetachedWord {
        model: String,
        document: String,
        page: u16,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
        word: String,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
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
    TranslationRoutes,
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
    DictionarySourceLocations {
        #[serde(rename = "cardId")]
        card_id: String,
    },
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
    RollbackTranslationPackage {
        source: String,
        target: String,
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
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        engine: Option<String>,
        speaker: u32,
        rate: f32,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    SynthesizeSpeechText {
        text: String,
        language: String,
        engine: String,
        speaker: u32,
        rate: f32,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    PrefetchSpeech {
        id: String,
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        engine: Option<String>,
        speaker: u32,
        rate: f32,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelSpeech {
        #[serde(rename = "requestId")]
        request_id: u64,
        #[serde(default)]
        prefetch: bool,
    },
    TranslateParagraph {
        id: String,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(default)]
        background: bool,
    },
    PdfPageTranslation {
        id: String,
        page: u16,
        target: String,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
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
        expected: Option<reader_core::translation_queue::TranslationWindowAddress>,
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
    TranslateDictionaryGloss {
        id: String,
        source: String,
        gloss: String,
        target: Option<String>,
    },
    TranslateDictionaryGlosses {
        id: String,
        source: String,
        target: String,
        glosses: Vec<String>,
    },
    TranslateSelection {
        id: String,
        target: String,
        selection: reader_core::translation::TranslationSelection,
    },
    LookupDetachedPdfWord {
        id: String,
        page: u16,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
        text: String,
    },
    SaveDetachedPdfWord {
        id: String,
        page: u16,
        input: reader_core::knowledge::DetachedVocabularyInput,
    },
    SaveWord {
        entry: Vocabulary,
        selection: Option<reader_core::dictionary_transfer::DictionarySelection>,
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
        settings: Option<reader_core::practice::PracticeSettings>,
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
