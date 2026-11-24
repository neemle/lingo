#!/bin/bash
set -euo pipefail

app="$1"
test -f "$app/Contents/Info.plist"
repo="$(cd "$(dirname "$0")/../.." && pwd)"
cache="$repo/target/ocr-macos"
downloads="$cache/downloads"
mkdir -p "$downloads"
work="$(mktemp -d "$cache/build.XXXXXXXX")"
trap 'rm -rf -- "$work"' EXIT

fetch() {
  local destination="$1" hash="$2" url="$3"
  if ! printf '%s  %s\n' "$hash" "$destination" | shasum -a 256 --check --status; then
    local temporary
    temporary="$(mktemp "$work/download.XXXXXXXX")"
    curl --fail --silent --show-error --location --retry 2 --max-time 180 "$url" --output "$temporary"
    printf '%s  %s\n' "$hash" "$temporary" | shasum -a 256 --check
    mv -f "$temporary" "$destination"
  fi
}

fetch "$downloads/libpng-1.6.54.tar.gz" ba7efce137409079989df4667706c339bebfbb10e9f413474718012a13c8cd4c \
  https://codeload.github.com/pnggroup/libpng/tar.gz/refs/tags/v1.6.54
fetch "$downloads/tiff-4.7.2.tar.gz" 672bd7d10aee4606171afb864f3570b83340f6a33e2c186dc0512f7145ffdf6a \
  https://download.osgeo.org/libtiff/tiff-4.7.2.tar.gz
fetch "$downloads/libjpeg-turbo-3.2.1.tar.gz" c27ad72a68da151177929aa969ae773a2da5ac2af9ce946079314fdac8cece7f \
  https://github.com/libjpeg-turbo/libjpeg-turbo/releases/download/3.2.1/libjpeg-turbo-3.2.1.tar.gz
fetch "$downloads/leptonica-1.86.0.tar.gz" b4447faf61a8786a9b211d58d4103d85d47fd3a5dd418d5a6bc525d41db54ccc \
  https://codeload.github.com/DanBloomberg/leptonica/tar.gz/refs/tags/1.86.0
fetch "$downloads/tesseract-5.5.2.tar.gz" 6235ea0dae45ea137f59c09320406f5888383741924d98855bd2ce0d16b54f21 \
  https://codeload.github.com/tesseract-ocr/tesseract/tar.gz/refs/tags/5.5.2

prefix="$work/prefix"
sdk="$(xcrun --sdk macosx --show-sdk-path)"
options=(-DCMAKE_BUILD_TYPE=Release -DCMAKE_OSX_ARCHITECTURES=arm64 -DCMAKE_OSX_DEPLOYMENT_TARGET=26.0
  "-DCMAKE_OSX_SYSROOT=$sdk" "-DCMAKE_INSTALL_PREFIX=$prefix" "-DCMAKE_PREFIX_PATH=$prefix"
  -DBUILD_SHARED_LIBS=OFF "-DZLIB_LIBRARY=$sdk/usr/lib/libz.tbd" "-DZLIB_INCLUDE_DIR=$sdk/usr/include")
jobs="$(sysctl -n hw.perflevel0.physicalcpu 2>/dev/null || sysctl -n hw.physicalcpu)"
if (( jobs > 8 )); then jobs=8; fi

for name in libpng-1.6.54 tiff-4.7.2 libjpeg-turbo-3.2.1 leptonica-1.86.0 tesseract-5.5.2; do
  tar -xzf "$downloads/$name.tar.gz" -C "$work"
done
# Reuse the built helper across installer runs on the same SDK.
fingerprint="$(shasum -a 256 "$0" | cut -d ' ' -f 1)"
helper="$cache/tesseract-arm64-$(xcrun --sdk macosx --show-sdk-version)-$fingerprint"
if [[ ! -x "$helper" ]]; then
  cmake -S "$work/libpng-1.6.54" -B "$work/png-build" "${options[@]}" \
    -DPNG_SHARED=OFF -DPNG_FRAMEWORK=OFF -DPNG_TESTS=OFF -DPNG_TOOLS=OFF
  cmake --build "$work/png-build" --target install --parallel "$jobs"
  # Leptonica decodes embedded TIFF fonts even when the OCR input is PNG.
  cmake -S "$work/tiff-4.7.2" -B "$work/tiff-build" "${options[@]}" \
    -Dtiff-static=ON -Dtiff-tools=OFF -Dtiff-tests=OFF -Dtiff-contrib=OFF -Dtiff-docs=OFF -Dtiff-cxx=OFF \
    -Djpeg=OFF -Djbig=OFF -Dlerc=OFF -Dlibdeflate=OFF -Dlzma=OFF -Dwebp=OFF -Dzstd=OFF
  cmake --build "$work/tiff-build" --target install --parallel "$jobs"
  # Leptonica compiles its TIFF decoder only when JPEG support is also present.
  cmake -S "$work/libjpeg-turbo-3.2.1" -B "$work/jpeg-build" "${options[@]}" \
    -DENABLE_SHARED=OFF -DENABLE_STATIC=ON -DWITH_TURBOJPEG=OFF -DWITH_SIMD=OFF
  cmake --build "$work/jpeg-build" --target install --parallel "$jobs"
  cmake -S "$work/leptonica-1.86.0" -B "$work/leptonica-build" "${options[@]}" \
    "-DPNG_LIBRARY=$prefix/lib/libpng16.a" "-DPNG_PNG_INCLUDE_DIR=$prefix/include" \
    "-DJPEG_LIBRARY=$prefix/lib/libjpeg.a" "-DJPEG_INCLUDE_DIR=$prefix/include" \
    "-DTIFF_LIBRARY=$prefix/lib/libtiff.a" "-DTIFF_INCLUDE_DIR=$prefix/include" \
    -DENABLE_GIF=OFF -DENABLE_JPEG=ON -DENABLE_TIFF=ON -DENABLE_WEBP=OFF -DENABLE_OPENJPEG=OFF
  cmake --build "$work/leptonica-build" --target install --parallel "$jobs"
  # Leptonica's exported static target refers to ZLIB::ZLIB when TIFF is enabled.
  printf '%s\n' 'find_package(ZLIB REQUIRED)' > "$work/tesseract-dependencies.cmake"
  cmake -S "$work/tesseract-5.5.2" -B "$work/tesseract-build" "${options[@]}" \
    "-DCMAKE_PROJECT_INCLUDE=$work/tesseract-dependencies.cmake" \
    -DBUILD_TRAINING_TOOLS=OFF -DGRAPHICS_DISABLED=ON \
    -DDISABLE_ARCHIVE=ON -DDISABLE_CURL=ON -DDISABLE_TIFF=ON
  cmake --build "$work/tesseract-build" --target tesseract --parallel "$jobs"

  candidate="$work/tesseract-build/bin/tesseract"
  # A packaged helper must also run on a Mac with no Homebrew or MacPorts.
  otool -L "$candidate" | awk 'NR > 1 && $1 !~ /^\/(usr\/lib|System\/Library)\// { print; failed = 1 } END { exit failed }'
  mv -f "$candidate" "$helper"
fi

resources="$app/Contents/Resources/OCR"
models="$resources/tessdata"
mkdir -p "$models/configs" "$resources/licenses" "$app/Contents/Helpers"
fast="$downloads/tessdata_fast-4.1.0"
mkdir -p "$fast"
while read -r hash name; do
  fetch "$fast/$name" "$hash" "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/4.1.0/$name"
  if [[ "$name" == *.traineddata ]]; then
    cp "$fast/$name" "$models/$name"
  else
    cp "$fast/$name" "$resources/licenses/tessdata_fast.txt"
  fi
done <<'MODELS'
cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 LICENSE
934bcaf97ef3348413263331131c9fa7f55f30db333c711929c124fb635f7e1b ces.traineddata
19d219bbb6672c869d20a9636c6816a81eb9a71796cb93ebe0cb1530e2cdb22d deu.traineddata
7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2 eng.traineddata
ced037562e8c80c13122dece28dd477d399af80911a28791a66a63ac1e3445ca fra.traineddata
b8f89e1e785118dac4d51ae042c029a64edb5c3ee42ef73027a6d412748d8827 ita.traineddata
ced0e5e046a84c908a6aa7accbef9a232c4a5d9a8276691b81c6ee64d02963f6 nld.traineddata
c4476cdbc0e33d898d32345122b7be1cbf85ace15f920f06c7714756e1ef79b2 pol.traineddata
c4932b937207a9514b7514d518b931a99938c02a28a5a5a553f8599ed58b7deb por.traineddata
9adfde6b51ba4b97efd10ea37c3070fd3fc2bad7815e81f5c3c198cd96216cc9 ron.traineddata
6f2e04d02774a18f01bed44b1111f2cd7f3ba7ac9dc4373cd3f898a40ea6b464 spa.traineddata
f7304988d41f833efebcc2d529df54b1903ecebbc3da1faabd19a0fddd4fe586 swe.traineddata
d59e53e2bded32f4445f124b4b00240fcac7e8044c003ab822ccb94f0b3db59b ukr.traineddata
MODELS
cp "$work/libpng-1.6.54/LICENSE" "$resources/licenses/libpng.txt"
cp "$work/tiff-4.7.2/LICENSE.md" "$resources/licenses/libtiff.txt"
cp "$work/libjpeg-turbo-3.2.1/LICENSE.md" "$resources/licenses/libjpeg-turbo.txt"
cp "$work/libjpeg-turbo-3.2.1/README.ijg" "$resources/licenses/libjpeg-ijg.txt"
cp "$work/leptonica-1.86.0/leptonica-license.txt" "$resources/licenses/leptonica.txt"
cp "$work/tesseract-5.5.2/LICENSE" "$resources/licenses/tesseract.txt"
cp "$work/tesseract-5.5.2/tessdata/configs/tsv" "$models/configs/tsv"
cp "$helper" "$app/Contents/Helpers/tesseract"
codesign --force --sign - "$app/Contents/Helpers/tesseract"
"$app/Contents/Helpers/tesseract" --tessdata-dir "$models" --list-langs
