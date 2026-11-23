#!/bin/bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  echo "Usage: $0 path/to/Reader.Native.app path/to/Lingo.pkg" >&2
  exit 2
fi

app="${1%/}"
package="$2"
plist="$app/Contents/Info.plist"
executable="$(plutil -extract CFBundleExecutable raw -o - "$plist")"
bundle_id="$(plutil -extract CFBundleIdentifier raw -o - "$plist")"
bundle_version="$(plutil -extract CFBundleVersion raw -o - "$plist")"
if [[ ! -x "$app/Contents/MacOS/$executable" || -z "$bundle_id" || -z "$bundle_version" ]]; then
  echo "Expected a built Mac Catalyst application bundle at $app" >&2
  exit 1
fi

staging="$(mktemp -d "${TMPDIR:-/tmp}/lingo-package.XXXXXXXX")"
trap 'rm -rf -- "$staging"' EXIT
payload="$staging/payload"
components="$staging/components.plist"
mkdir "$payload"
staged_app="$payload/Lingo.app"
ditto "$app" "$staged_app"

# Developer ID signatures must cover the helper and models at signing time.
signature="$(codesign --display --verbose=2 "$staged_app" 2>&1)" || true
if [[ ! -x "$staged_app/Contents/Helpers/tesseract" ]]; then
  if [[ "$signature" == *Authority=* ]]; then
    echo "Bundle OCR with bundle-macos-ocr.sh before signing the application." >&2
    exit 1
  fi
  bash "$(dirname "$0")/bundle-macos-ocr.sh" "$staged_app"
  codesign --force --sign - "$staged_app"
fi

# The linker's ad-hoc signature covers only the executable. Seal the staged bundle for Finder.
if ! codesign --verify --deep --strict "$staged_app" 2>"$staging/verify.log"; then
  signature="$(codesign --display --verbose=2 "$staged_app" 2>&1)" || true
  if [[ "$signature" == *linker-signed* || "$signature" == *'code object is not signed at all'* ]]; then
    codesign --force --sign - "$staged_app"
  else
    cat "$staging/verify.log" >&2
    exit 1
  fi
  codesign --verify --deep --strict "$staged_app"
fi

pkgbuild --analyze --root "$payload" "$components"
# Keep the installer from relocating to an indexed build copy of the bundle.
plutil -replace 0.BundleIsRelocatable -bool false "$components"
pkgbuild --root "$payload" --component-plist "$components" \
  --identifier "$bundle_id.pkg" --version "$bundle_version" --install-location /Applications "$package"
pkgutil --payload-files "$package" | awk -v entry="./Lingo.app/Contents/MacOS/$executable" \
  '$0 == entry { app = 1 }
   $0 == "./Lingo.app/Contents/Helpers/tesseract" { ocr = 1 }
   /^\.\/Lingo\.app\/Contents\/Resources\/OCR\/tessdata\/[a-z][a-z][a-z]\.traineddata$/ { models++ }
   END { exit !(app && ocr && models == 12) }'
