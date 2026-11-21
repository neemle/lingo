# Initial delivery

The attached v0.1 specification is the product reference. This bootstrap delivers a bounded desktop slice, with unavailable capabilities shown explicitly. No model downloads or cloud content processing are enabled.

Implementation: Angular 22, Tauri 2, Rust core, SQLite. Import TXT, DOCX and text PDF; two-column reader with a labelled French/Ukrainian sample, sentence navigation, local vocabulary, independent directional reviews, JSON export/import. Original file bytes are retained. macOS speech uses the local system `say` command; Windows uses System.Speech. Word boundaries and OCR are subsequent native-adapter work unless verified separately. User documents have no machine translation until a validated offline package is installed. Browser preview persists locally and is explicitly distinguished from the desktop SQLite store.

Verification: core Unicode, import and persistence/review/backup tests; Angular production compilation; Rust desktop compilation; interactive reader smoke. Windows/macOS distribution, scanned PDF, OCR, model installation and memory quality gates remain open. No claim of completing M0–M9.
