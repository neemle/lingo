# Lingo Reader — Читай

Native desktop language reader with a Ukrainian interface: MAUI XAML/C# on Windows (WinUI 3) and
macOS (Mac Catalyst), backed by Rust and local SQLite. Requirements and work tracking live in LeaseVibe MCP.

The reader imports UTF-8 TXT, DOCX and text PDFs up to 20 MiB, retains originals, restores sentence progress,
and saves manual translations with context. Forward/reverse practice states and JSON backups use the same Rust core.
Translation models, OCR and grammar packages are not installed; the app reports their absence.

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

Speech uses system voices and reports missing languages. Precise word timing, OCR, automatic offline translation,
mobile releases, signed installers and the 4 GiB hardware target remain unverified or unimplemented.
