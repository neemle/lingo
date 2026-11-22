# Lingo Reader — Читай

Native desktop language reader with a Ukrainian interface: MAUI XAML/C# on Windows (WinUI 3) and
macOS (Mac Catalyst), backed by Rust and local SQLite. Requirements and work tracking live in LeaseVibe MCP.

The reader imports UTF-8 TXT, DOCX, PDFs and supported photographs up to 20 MiB, retains originals, restores sentence progress,
and saves manual translations with context. Forward/reverse practice states and JSON backups use the same Rust core.
Translation and grammar packages are optional and installed separately. The app reports unavailable OCR and missing packages.

## Build and run

Use an installed .NET SDK that supports the `net10.0` targets, its compatible MAUI workload, and Rust.
The Rust build also uses CMake and libclang for llama.cpp. Windows requires MSVC and the Windows SDK. Mac Catalyst requires full Xcode compatible with the installed
.NET Apple workload; Command Line Tools alone cannot build or launch the app. No Node or browser runtime is used.
Run commands from the repository root.

macOS, Apple Silicon:

```sh
rustup target add aarch64-apple-ios-macabi
CMAKE_OSX_SYSROOT="$(xcrun --sdk macosx --show-sdk-path)" \
  CFLAGS_aarch64_apple_ios_macabi="-target arm64-apple-ios15.0-macabi" \
  CXXFLAGS_aarch64_apple_ios_macabi="-target arm64-apple-ios15.0-macabi" \
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
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build -p reader-ffi --release --locked --target x86_64-pc-windows-msvc
$readerProject = "apps/reader-native/Reader.Native/Reader.Native.csproj"
dotnet build $readerProject -f net10.0-windows10.0.19041.0 `
  -c Release -p:RuntimeIdentifier=win-x64
dotnet run --project $readerProject -f net10.0-windows10.0.19041.0 `
  -c Release -p:RuntimeIdentifier=win-x64 --no-build
```

The Windows head copies the matching Rust DLL beside the application. Rust remains the only database writer.
The app stores its database in its local application-data directory. Export a backup before changing devices.

To build an installable x64 MSIX with Windows OCR, use the Rust build above and a code-signing
certificate in the current user's Personal store with the subject `CN=Lingo Reader`.

```powershell
$readerProject = "apps/reader-native/Reader.Native/Reader.Native.csproj"
dotnet publish $readerProject -f net10.0-windows10.0.19041.0 -c Release `
  -p:LingoMsix=true -p:RuntimeIdentifierOverride=win-x64 `
  "-p:PackageCertificateThumbprint=$thumbprint"
Add-AppxPackage -Path $msixPath
```

Set `$thumbprint` to the signing certificate's thumbprint and `$msixPath` to the MSIX generated
under `bin/Release`. The package includes the x64 Rust DLL, .NET and Windows App SDK runtimes.
The destination must trust the signing certificate. For a local test certificate, follow the
[Microsoft MAUI signing instructions](https://learn.microsoft.com/en-us/dotnet/maui/windows/deployment/publish-cli).

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
Practice restores the active question, mode, direction, choice order, and submitted answer after reopening.
Grading clears that question in the same transaction that records the review. A backup restores an unfinished
question into a fresh library.

Speech uses installed system voices, supports pause/resume, and can advance through sentences while saving progress.
The reader remembers the voice per source language, speed, and continuous reading setting.
If the voice supplies word boundaries, **Продовжити зі збереженого слова** resumes at the last saved word
after reopening. The checkpoint is included in backups and discarded if the sentence text changes.
Individual words are highlighted only when the voice provides matching boundary events. Additional offline translation pairs,
mobile releases, signed installers and the 4 GiB hardware target remain unverified or unimplemented.

## Photo and PDF OCR

Choose the photo's language in the library, then open a JPEG, PNG, WebP or HEIF image supported
by the operating system. The app reads the image on-device, saves the recognized text, and keeps the
original. The **Оригінал** action shows a preview with optional line overlays. Archive restore preserves the
recognized text and line positions. In the preview, choose a recognized line and select **Виправити**
or **Скинути**. Line corrections change the reading text while retaining the original image/PDF,
recognition confidence, and raw OCR output. An edit that would detach an annotated sentence is rejected.
With a grammar package installed, **Підказки** shows observed alternatives for unfamiliar words in the selected
line. Select a replacement to apply it. The reader checks for common OCR confusions and one-character edits.

For a scanned or mixed PDF, use **Розпізнати PDF** in the library. It reads each available text layer
and runs OCR only on pages without extractable text, up to 40 pages. The original is preserved, and
the reader can open the source page for the active sentence. Ordinary PDF import still reads only
the embedded text layer.

Mac Catalyst uses Apple Vision. The installed Windows MSIX uses the system recognition language for OCR.
The unpackaged Windows build can read and preview photos restored from a backup.

## Offline translation packages

The reader shows which route is available and sends you to the needed download when a package is missing.
Choose a language pair in **Налаштування** and select **Завантажити пакет**. The download can be stopped
and resumed. Translation uses local files after installation.
Before a download, the app checks space for the remaining files, temporary archive, and installed model.
It checks the destination again before extraction.
Use **Видалити пакет перекладу** to free its local files. You can download the package again from Settings.
If installed files are damaged, **Завантажити пакет повторно** verifies a replacement before switching to it.
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
Translation can join consecutive wrapped lines in a PDF or OCR transcript. The pane marks joined input,
and the source text, bookmarks, and current reading position remain intact.

## Optional local model files

In **Налаштування → Додаткові локальні моделі**, choose a model to download, verify or remove.
Stopping a download keeps the partial files so the next attempt can resume. Each completed file must match
its pinned SHA-256 before the model is activated. An interrupted replacement preserves the previous files.
Select a word and choose **Пояснити з Qwen** for a local, generated explanation. The app checks available
memory before loading the model and labels the result with the model name. Qwen is optional for reading,
translation and the vocabulary. Semantic search with E5 is pending.

- [Qwen3.5-0.8B GGUF](https://huggingface.co/ggml-org/Qwen3.5-0.8B-GGUF/tree/004fd086eccea9f7cf867aa07ba328773beef81d), four-bit, Apache-2.0.
- [Qwen3.5-2B GGUF](https://huggingface.co/unsloth/Qwen3.5-2B-GGUF/tree/f6d5376be1edb4d416d56da11e5397a961aca8ae), four-bit, Apache-2.0.
- [Multilingual E5 Small ONNX](https://huggingface.co/intfloat/multilingual-e5-small/tree/614241f622f53c4eeff9890bdc4f31cfecc418b3/onnx), MIT.

## Offline word analysis

In **Налаштування**, select **Встановити або оновити пакет аналізу** and choose a language. The reader verifies the
corpus against its pinned SHA-256 and builds a local read-only index. Select a word in the source to see
observed lemmas, parts of speech, grammatical features and relative frequency. If there are multiple
readings, the card shows the most frequent variant for each before showing additional variants.
The card names the corpus and marks missing forms. Analysis leaves the original and reading text intact.
Current indexes can rank readings using observed words immediately before and after the selection.
The card reports matching corpus occurrences and keeps other readings available. Earlier indexes retain
frequency-based lookup and can be updated from Settings.
The latest index also uses annotated fixed expressions and verb particles. Select a part of the expression
to see matches such as **turned … off** and their corpus counts.
Choose the expression when saving a card to practice its canonical form. The card retains the occurrence
and its sentence even after the language index is replaced.
In **Форма для словникової картки**, choose the reading you intend to save. Equivalent inflections
of the same lemma, part of speech and reviewed translation share one card and its review history.
The card retains the recorded form, source sentence and the grammatical data available when saved.

The initial indexes use the training annotations from Universal Dependencies 2.18 and omit corpus passages.
They can be reinstalled independently of documents and backup data.

- English uses [UD English EWT](https://github.com/UniversalDependencies/UD_English-EWT/tree/b7711cce01cdd4f5fcc0a8199b8a50d951b16c0c), CC BY-SA 4.0 for annotations.
- French uses [UD French GSD](https://github.com/UniversalDependencies/UD_French-GSD/tree/15958a5df1d09e2a360784caaed07b9a3fcbf7a8), CC BY-SA 4.0 for annotations.
- Ukrainian uses [UD Ukrainian IU](https://github.com/UniversalDependencies/UD_Ukrainian-IU/tree/0d5ed088e959ccfbd933d768114d8503c32c9b37), CC BY-NC-SA 4.0.
