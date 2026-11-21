# Verification record — 21 November 2026

Environment: macOS, Apple Silicon (arm64), Node 24.21, Rust 1.95. Results apply to this initial working slice.

- `npm run build`: passed; production frontend approximately 277 kB before compression.
- `cargo test -p reader-core --locked`: 5 tests passed. Unicode offsets (including combining marks, emoji, Arabic and CJK), stable segmentation IDs, TXT persistence/restart/deduplication, DOCX entities and paragraph extraction, text PDF import, retained originals, separate directional learning states, backup integrity/recovery and review-history restoration.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `npm run test:ui`: 3 Playwright workflows passed in installed Chrome. Reading, word-card save, independent forward/reverse reviews, import, position recovery, operations with the browser context offline after loading, narrow layout without horizontal overflow, honest unsupported import errors, and backup restoration in a fresh browser context.
- `npm run desktop:build -- --bundles app`: passed; native arm64 macOS application bundle approximately 12.5 MiB.
- Native macOS app launched through its packaged `.app`, without the frontend dev server. The saved reading position survived app quit/relaunch. System French speech, sentence advancement, repeat and stop were exercised. A native `say` process with the selected voice was observed. This does not independently measure audio quality or word timing.
- Native Save dialog exported the retained sample original to `artifacts/verified-original.txt`; its UTF-8 text matched the source exactly.
- Visual inspection in the native macOS WebView caught an icon sizing issue not present in Chrome. Static global icon styles fixed it; the final native screenshot was checked.

Not validated: Windows execution (workflow provided), x86 macOS, signed/notarized installer, 4 GB device memory profile, production OCR/translation/model downloads, precise speech word boundaries, or the complete release definition of done. Browser reload requires the local frontend server; the packaged desktop app embeds its frontend and has no such dependency. Browser preview data is separate from desktop SQLite data.
