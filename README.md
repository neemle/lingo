# Lingo Reader

Offline-first language reader, bootstrapped from [the specification authority reference](SPECIFICATION.md). Ukrainian interface, Angular 22 + Tauri 2 + Rust + local SQLite.

## Run

Requires Node 24.15+ and Rust with platform Tauri prerequisites (Xcode Command Line Tools on macOS; MSVC Build Tools and WebView2 on Windows).

```sh
npm ci
npm run desktop
```

Browser preview: `npm start`, then open http://127.0.0.1:1420. This preview uses browser storage and supports TXT; the desktop application uses SQLite and supports TXT, DOCX and text PDFs. Browser storage can be cleared by the browser, so export backups for durable use.

```sh
npm run desktop:build -- --bundles app  # macOS
npm run desktop:build -- --bundles nsis # Windows, on Windows
npm run check
npm run test:ui
```

The UI test configuration uses the installed Google Chrome on macOS; override its executable path for other hosts. CI compiles native desktop code on macOS and Windows. A committed lockfile pins resolved dependencies.

## Available in this slice

- Two aligned reading columns with a clearly labelled French/Ukrainian sample.
- TXT, DOCX and text PDF import on desktop, retained originals, SHA-256 deduplication, stable sentence IDs and UTF-8/UTF-16 spans.
- Sentence selection and navigation, progress recovery after restart.
- Sentence-level system speech: local macOS `say`, Windows System.Speech adapter. Only locally detected voices are offered. Stop/restart playback resumes at the current sentence.
- Context word cards and an authored demo lexicon; manual translations for other words. Saved words retain their source sentence.
- Forward and reverse quizzes with independent intervals, self-rating and typed answers. The small deterministic repetition algorithm is not FSRS.
- Versioned JSON backups with source files, vocabulary, directional states and review history; integrity checking and transactional restore on desktop.
- Resource screen showing actual voice availability and missing translation/OCR/model packages. No automatic downloads and no cloud document processing.

## Still open

This is a first working slice, **not the completed M0–M9 release**. OCR/photo/scanned PDF import, original-page rendering, verified word timing, production offline translation, model management, deep grammar packs, full native sharing, mobile builds and the 4 GB hardware gate remain unimplemented/unverified. Original files can currently be downloaded for opening in their native viewer. Windows native adapters and CI are provided but have not been executed on this Mac. French demo translations are fixtures, not a translation engine; the UI labels them accordingly. Installing OS voices is managed by the operating system.

The MCP control record preserves the non-normative historical implementation and verification companions. See [the authority reference](SPECIFICATION.md).
