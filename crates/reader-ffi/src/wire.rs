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
    ParseOcrTsv {
        tsv: String,
        width: u32,
        height: u32,
        #[serde(default, rename = "sparseTsv")]
        sparse_tsv: Option<String>,
        #[serde(default, rename = "imageRotation")]
        image_rotation: Option<u16>,
    },
    MergeRotatedOcr {
        primary: Vec<reader_core::ocr::OcrLine>,
        native: Vec<reader_core::ocr::OcrLine>,
        #[serde(default, rename = "pageSize")]
        page_size: Option<[u32; 2]>,
    },
    SamplePdfBackgrounds {
        rgba: String,
        width: usize,
        height: usize,
        boxes: Vec<reader_core::pdf_appearance::SampleBox>,
        #[serde(default, rename = "withInk")]
        with_ink: bool,
    },
    SeparatePdfBlocks {
        text: String,
        words: Vec<reader_core::pdf_text_blocks::PositionedWord>,
        #[serde(rename = "pageWidth")]
        page_width: f64,
    },
    PlanPdfTranslationLayout {
        rows: Vec<reader_core::pdf_translation_layout::SourceRow>,
        geometry: Vec<reader_core::ocr::OcrLine>,
        offset: usize,
        styles: Vec<reader_core::pdf_translation_layout::SourceStyleAnchor>,
        #[serde(rename = "preserveArtwork")]
        preserve_artwork: bool,
    },
    FindPdfLigatureJoins {
        text: String,
        words: Vec<reader_core::pdf_text_blocks::PositionedWord>,
    },
    StorageUsage,
    ClearStorage {
        scope: reader_core::storage_management::StorageScope,
    },
    AdvanceBackgroundTranslation {
        id: String,
        revision: String,
        target: String,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelBackgroundTranslation {
        #[serde(rename = "requestId")]
        request_id: u64,
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
    SaveIntakeSourceLanguage {
        id: String,
        language: String,
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
    PrepareFrenchSpeech {
        text: String,
    },
    PrepareUkrainianSpeech {
        text: String,
    },
    DetectLanguage {
        text: String,
        #[serde(default, rename = "includeSample")]
        include_sample: bool,
    },
    DetectDocumentLanguage {
        name: String,
        bytes: Vec<u8>,
        #[serde(default, rename = "includeSample")]
        include_sample: bool,
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
        #[serde(rename = "deferredFrom")]
        deferred_from: Option<u16>,
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
    HasUnrecognizedPdfSearchPages {
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
    ReadDetachedPdfSearchPages {
        id: String,
        #[serde(rename = "startPage")]
        start_page: u16,
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
        #[serde(default, rename = "retryNow")]
        retry_now: bool,
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
        #[serde(rename = "expectedText")]
        expected_text: Option<String>,
        lines: Vec<reader_core::ocr::OcrLine>,
    },
    SupplementPdfOcrPage {
        id: String,
        page: u16,
        #[serde(rename = "revisionId")]
        revision_id: String,
        language: String,
        detached: bool,
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
    RepairStoredPdfTextOrder {
        id: String,
        page: u16,
        #[serde(rename = "revisionId")]
        revision_id: String,
        reference: String,
        changes: Vec<reader_core::PdfTextOrderEdit>,
    },
    UpgradeStoredOcrLayout {
        id: String,
        page: u16,
        #[serde(rename = "revisionId")]
        revision_id: String,
        reference: String,
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
        revision: Option<String>,
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
        page: Option<u16>,
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
    ClearSpeechResume {
        id: String,
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "textHash")]
        text_hash: String,
        #[serde(rename = "sourceLanguage")]
        source_language: String,
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
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(rename = "sourceLanguage")]
        source_language: Option<String>,
        #[serde(default)]
        regenerate: bool,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    TranslateOfflinePassage {
        id: String,
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        target: String,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(rename = "sourceLanguage")]
        source_language: Option<String>,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
        #[serde(default, rename = "marianOnly")]
        marian_only: bool,
    },
    CancelOfflinePassage {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    PassageSource {
        id: String,
        page: Option<u16>,
        #[serde(rename = "sentenceId")]
        sentence_id: String,
        #[serde(rename = "revisionId")]
        revision_id: Option<String>,
        #[serde(rename = "sourceLanguage")]
        source_language: Option<String>,
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
        #[serde(default, rename = "explainSyntax")]
        explain_syntax: bool,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelSentenceReview {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    ReviewOcrWord {
        id: String,
        page: Option<u16>,
        #[serde(rename = "revisionId")]
        revision_id: String,
        #[serde(rename = "startUtf16")]
        start_utf16: usize,
        word: String,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
    },
    ReviewOcrLine {
        id: String,
        page: Option<u16>,
        #[serde(rename = "revisionId")]
        revision_id: String,
        index: usize,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
    },
    ReviewOcrPage {
        id: String,
        page: u16,
        detached: bool,
        #[serde(rename = "revisionId")]
        revision_id: String,
        #[serde(default, rename = "cachedOnly")]
        cached_only: bool,
        #[serde(default, rename = "reviewedIndices")]
        reviewed_indices: Vec<usize>,
        #[serde(default, rename = "searchCachedPage")]
        search_cached_page: bool,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelOcrPageReview {
        #[serde(rename = "requestId")]
        request_id: u64,
    },
    ApplyOcrPageReview {
        id: String,
        page: u16,
        detached: bool,
        #[serde(rename = "revisionId")]
        revision_id: String,
        edits: Vec<reader_core::ocr::OcrLineEdit>,
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
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
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
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelWordExplanation {
        #[serde(rename = "requestId")]
        request_id: u64,
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
    RemoveKnowledgePackage { language: String },
    RemoveLexicalPackage { language: String },
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
    TranslationGlossMatches {
        language: String,
        gloss: String,
        texts: Vec<String>,
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
        #[serde(default, rename = "compactAudio")]
        compact_audio: bool,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    SynthesizeSpeechText {
        text: String,
        language: String,
        engine: String,
        speaker: u32,
        rate: f32,
        #[serde(default, rename = "compactAudio")]
        compact_audio: bool,
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
        #[serde(rename = "refreshAtUtf16")]
        refresh_at: Option<usize>,
        refresh: Option<reader_core::PdfSentenceRefresh>,
        source: Option<reader_core::PdfTranslationSource>,
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelPdfPageTranslation {
        #[serde(rename = "requestId")]
        request_id: u64,
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
        #[serde(default, rename = "startOffset")]
        start_offset: usize,
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
        #[serde(default)]
        regenerate: bool,
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
        #[serde(rename = "requestId")]
        request_id: Option<u64>,
    },
    CancelSelectionTranslation {
        #[serde(rename = "requestId")]
        request_id: u64,
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
    SavePracticeDraft {
        revision: u64,
        draft: String,
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
    CheckBackupStagingSpace {
        directory: String,
        bytes: u64,
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
