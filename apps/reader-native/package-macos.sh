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
if [[ ! -x "$app/Contents/MacOS/$executable" || -z "$bundle_id" ]]; then
  echo "Expected a built Mac Catalyst application bundle at $app" >&2
  exit 1
fi

staging="$(mktemp -d "${TMPDIR:-/tmp}/lingo-package.XXXXXXXX")"
trap 'rm -rf -- "$staging"' EXIT
payload="$staging/payload"
components="$staging/components.plist"
mkdir "$payload"
ditto "$app" "$payload/Lingo.app"
pkgbuild --analyze --root "$payload" "$components"
# Keep the installer from relocating to an indexed build copy of the bundle.
plutil -replace 0.BundleIsRelocatable -bool false "$components"
pkgbuild --root "$payload" --component-plist "$components" \
  --identifier "$bundle_id.pkg" --install-location /Applications "$package"
pkgutil --payload-files "$package" | awk -v entry="./Lingo.app/Contents/MacOS/$executable" \
  '$0 == entry { found = 1 } END { exit !found }'
