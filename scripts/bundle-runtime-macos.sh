#!/usr/bin/env bash
# Builds a self-contained OCR runtime (Python + ocrmypdf, Tesseract, pngquant)
# into src-tauri/resources/runtime so it can be shipped inside the
# Tauri app bundle. Ghostscript is intentionally not included (AGPL); ocrmypdf
# rasterizes with pypdfium2 and builds PDF/A with pikepdf instead. Homebrew binaries are copied and their dylibs relocated with
# dylibbundler so the installed app does not need Homebrew on the target Mac.
#
# Requirements (build machine only): brew, uv, dylibbundler
#   brew install tesseract pngquant dylibbundler uv
#
# Env:
#   PYTHON_VERSION   (default 3.13)
#   OCRMYPDF_VERSION (default: latest)
#   TESS_LANGS       space separated tesseract languages (default "eng osd").
#                    Languages not shipped by Homebrew are downloaded from
#                    tesseract-ocr/tessdata_fast.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/src-tauri/resources/runtime"
PYTHON_VERSION="${PYTHON_VERSION:-3.13}"
TESS_LANGS="${TESS_LANGS:-eng osd}"
OCRMYPDF_SPEC="ocrmypdf${OCRMYPDF_VERSION:+==$OCRMYPDF_VERSION}"

for tool in brew uv dylibbundler; do
  command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 1; }
done
for formula in tesseract pngquant; do
  brew list --versions "$formula" >/dev/null || { echo "error: brew install $formula" >&2; exit 1; }
done

echo "==> Cleaning $OUT"
rm -rf "$OUT"
mkdir -p "$OUT"/{bin,lib,tessdata}

# ---------------------------------------------------------------- Python
echo "==> Python $PYTHON_VERSION (python-build-standalone via uv)"
uv python install "$PYTHON_VERSION" >/dev/null
PY_BIN="$(uv python find "$PYTHON_VERSION")"
PY_HOME="$(cd "$(dirname "$PY_BIN")/.." && pwd -P)"
cp -R "$PY_HOME" "$OUT/python"
rm -f "$OUT/python/lib/python$PYTHON_VERSION/EXTERNALLY-MANAGED"
PY="$OUT/python/bin/python3"

echo "==> Installing $OCRMYPDF_SPEC"
uv pip install --quiet --python "$PY" --no-cache "$OCRMYPDF_SPEC"

echo "==> Pruning Python"
PYLIB="$OUT/python/lib/python$PYTHON_VERSION"
rm -rf "$PYLIB"/{test,idlelib,tkinter,turtledemo,ensurepip,lib2to3} \
       "$PYLIB"/config-* "$OUT/python/include" "$OUT/python/share" \
       "$OUT/python/lib"/{libtcl*,libtk*,tcl*,tk*,itcl*,thread*} 2>/dev/null || true
rm -f "$PYLIB"/lib-dynload/_tkinter*.so
find "$OUT/python" -name '__pycache__' -type d -prune -exec rm -rf {} +
find "$OUT/python" -name 'tests' -path '*site-packages*' -type d -prune -exec rm -rf {} +
# Remove pip/uv console scripts: they embed absolute shebangs that would be wrong once shipped.
find "$OUT/python/bin" -type f ! -name 'python3*' -delete
find "$OUT/python/bin" -type l ! -name 'python3*' -delete

# ---------------------------------------------------------------- Binaries
echo "==> Copying binaries"
for exe in tesseract pngquant; do
  cp "$(readlink -f "$(brew --prefix)/bin/$exe")" "$OUT/bin/$exe"
  chmod u+w "$OUT/bin/$exe"
done

echo "==> Relocating dylibs"
dylibbundler -of -b -cd \
  -x "$OUT/bin/tesseract" -x "$OUT/bin/pngquant" \
  -d "$OUT/lib" -p @executable_path/../lib/ >/dev/null

# ---------------------------------------------------------------- Data
echo "==> Tesseract data: $TESS_LANGS"
TESS_SHARE="$(brew --prefix tesseract)/share/tessdata"
cp -R "$TESS_SHARE/configs" "$TESS_SHARE/tessconfigs" "$OUT/tessdata/" 2>/dev/null || true
cp "$TESS_SHARE/pdf.ttf" "$OUT/tessdata/"
for lang in $TESS_LANGS; do
  if [[ -f "$TESS_SHARE/$lang.traineddata" ]]; then
    cp "$TESS_SHARE/$lang.traineddata" "$OUT/tessdata/"
  else
    echo "    downloading $lang"
    curl -fsSL -o "$OUT/tessdata/$lang.traineddata" \
      "https://github.com/tesseract-ocr/tessdata_fast/raw/main/$lang.traineddata"
  fi
done

# ---------------------------------------------------------------- Signing
# install_name_tool invalidates signatures; arm64 macOS refuses to run unsigned
# code, so ad-hoc sign every Mach-O file. (Release builds re-sign with your
# Developer ID; see README.)
echo "==> Ad-hoc signing"
while IFS= read -r -d '' f; do
  if file -b "$f" | grep -q 'Mach-O'; then
    codesign --force --sign - "$f" 2>/dev/null
  fi
done < <(find "$OUT" -type f \( -perm -u+x -o -name '*.dylib' -o -name '*.so' \) -print0)

# ---------------------------------------------------------------- Verify
echo "==> Verifying runtime with Homebrew hidden (sandbox)"
"$ROOT/scripts/verify-runtime-macos.sh"

du -sh "$OUT"
echo "Runtime ready at $OUT"
