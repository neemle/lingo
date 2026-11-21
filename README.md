# Lingo Reader — Читай

Native desktop language reader with a Ukrainian interface: MAUI XAML/C# on Windows (WinUI 3) and
macOS (Mac Catalyst), backed by Rust and local SQLite. Requirements and work tracking live in LeaseVibe MCP.

The reader imports UTF-8 TXT, DOCX, PDFs and supported photographs up to 20 MiB, retains originals, restores sentence progress,
and saves manual translations with context. Forward/reverse practice states and JSON backups use the same Rust core.
Translation models are optional and installed separately. The app reports unavailable OCR and grammar features.

## Build and run

Use an installed .NET SDK that supports the `net10.0` targets, its compatible MAUI workload, and Rust.
Windows requires MSVC and the Windows SDK. Mac Catalyst requires full Xcode compatible with the installed
.NET Apple workload; Command Line Tools alone cannot build or launch the app. No Node or browser runtime is used.
Run commands from the repository root.

macOS, Apple Silicon:

```sh
rustup target add aarch64-apple-ios-macabi
cargo build -p reader-ffi --release --locked --target aarch64-apple-ios-macabi
reader_project=apps/reader-native/Reader.Native/Reader.Native.csproj
dotnet build "$reader_project" -f net10.0-maccatalyst -p:EnableCodeSigning=false
dotnet build "$reader_project" -t:Run -f net10.0-maccatalyst -p:EnableCodeSigning=false
```

Catalyst links the Rust static library built for `aarch64-apple-ios-macabi`; a regular macOS dylib is incompatible.
The current native macOS head targets Apple Silicon.

Windows, x64:

```powershell
rustup target add x86_64-pc-windows-msvc
cargo build -p reader-ffi --release --locked --target x86_64-pc-windows-msvc
$readerProject = "apps/reader-native/Reader.Native/Reader.Native.csproj"
dotnet build $readerProject -f net10.0-windows10.0.19041.0 `
  -c Release -p:RuntimeIdentifier=win-x64
dotnet run --project $readerProject -f net10.0-windows10.0.19041.0 `
  -c Release -p:RuntimeIdentifier=win-x64 --no-build
```

The Windows head copies the matching Rust DLL beside the application. Rust remains the only database writer.
The app stores its database in its local application-data directory. Export a backup before changing devices.

## Checks and current limits

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
dotnet build apps/reader-native/Reader.Presentation/Reader.Presentation.csproj
```

Headless interop has exercised document imports, original bytes, restart progress, vocabulary, independent review
directions and backup/restore. Native C#/XAML compilation has passed locally for Catalyst; full local packaging is
blocked by absent Xcode. Native CI builds and launched Windows/macOS selection, accessibility and speech checks
remain separate gates. The explicit text/voice probe is available in Settings.

Speech uses system voices and reports missing languages. Precise word timing, automatic OCR suggestions, additional offline translation pairs,
mobile releases, signed installers and the 4 GiB hardware target remain unverified or unimplemented.

## Photo and PDF OCR

Choose the photo's language in the library, then open a JPEG, PNG, WebP or HEIF image supported
by the operating system. The app reads the image on-device, saves the recognized text, and keeps the
original. The **Оригінал** action shows a preview with optional line overlays. Archive restore preserves the
recognized text and line positions. In the preview, choose a recognized line and select **Виправити**
or **Скинути**. Line corrections change the reading text while retaining the original image/PDF,
recognition confidence, and raw OCR output. An edit that would detach an annotated sentence is rejected.

For a scanned or mixed PDF, use **Розпізнати PDF** in the library. It reads each available text layer
and runs OCR only on pages without extractable text, up to 40 pages. The original is preserved, and
the reader can open the source page for the active sentence. Ordinary PDF import still reads only
the embedded text layer.

Mac Catalyst uses Apple Vision. Windows OCR uses the installed system recognition language and requires
MSIX package identity. The unpackaged Windows build can read and preview photos restored from a backup.

## Offline translation packages

Choose a language pair in **Налаштування** and select **Завантажити пакет**. The download can be stopped
and resumed. Translation uses local files after installation.
For offline setup, the app also accepts a ZIP prepared from the pinned Helsinki-NLP packages below.
Download `config.json`, `source.spm`, `target.spm`, `vocab.json` and `model.safetensors` from the same revision.

- [French → Ukrainian](https://huggingface.co/Helsinki-NLP/opus-mt-fr-uk/tree/e789f8670b3515ed604605d80d108dd97d779848)
- [Ukrainian → French](https://huggingface.co/Helsinki-NLP/opus-mt-uk-fr/tree/dea3717a7169bffc2ff692e04af8a21fd1c20cae)
- [English → French](https://huggingface.co/Helsinki-NLP/opus-mt-en-fr/tree/c96fcd7f38a4c0d1ac9b61e255b47f3f7ff55e5e)
- [French → English](https://huggingface.co/Helsinki-NLP/opus-mt-fr-en/tree/c4aed37b318c763fd177aa449b44e3b783cc6c02)
- [English → Ukrainian](https://huggingface.co/Helsinki-NLP/opus-mt-en-uk/tree/55104bebd23cd2d87761e650aa6ac7369b379d1c)

Zip just those five files at the archive root. On macOS, run
`zip -0 model.zip config.json source.spm target.spm vocab.json model.safetensors` in the download directory.
On Windows, select the five files and compress them to ZIP. In the app, open **Налаштування →
Встановити мовний пакет із ZIP**. The app verifies the files against the pinned SHA-256 values before
installing the package. Open the reader, select the target language and choose **Перекласти офлайн**.
With French → English and English → Ukrainian installed, the reader can also translate French → Ukrainian
through English. The translation panel marks that route. Installing French → Ukrainian enables the direct route.
