#!/usr/bin/env bash
# Runs the bundled runtime inside a sandbox that blocks Homebrew and uv paths,
# proving nothing leaks in from the build machine. Mirrors the environment the
# app sets up in src-tauri/src/runtime.rs.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
RT="${1:-$ROOT/src-tauri/resources/runtime}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cat > "$WORK/profile.sb" <<EOF
(version 1)
(allow default)
(deny file-read* (subpath "/opt/homebrew"))
(deny file-read* (subpath "/usr/local"))
(deny file-read* (subpath "$HOME/.local/share/uv"))
EOF

run() {
  env -i HOME="$HOME" TMPDIR="${TMPDIR:-/tmp}" \
    PATH="$RT/bin:/usr/bin:/bin" \
    PYTHONHOME="$RT/python" PYTHONNOUSERSITE=1 PYTHONDONTWRITEBYTECODE=1 \
    TESSDATA_PREFIX="$RT/tessdata" \
    sandbox-exec -f "$WORK/profile.sb" "$@"
}

echo "-- tesseract"; run "$RT/bin/tesseract" --list-langs 2>&1 | tail -n +2 | tr '\n' ' '; echo
echo "-- pngquant";  run "$RT/bin/pngquant" --version
if [[ -e "$RT/bin/gs" ]]; then echo "error: Ghostscript must not be bundled" >&2; exit 1; fi
echo "-- ocrmypdf";  run "$RT/python/bin/python3" -m ocrmypdf --version

echo "-- end-to-end OCR"
run "$RT/python/bin/python3" - "$WORK/in.pdf" <<'PY'
import sys
from PIL import Image, ImageDraw, ImageFont
img = Image.new("L", (1700, 2200), 255)
d = ImageDraw.Draw(img)
font = ImageFont.load_default(size=64)
d.text((150, 300), "Bulk PDF Toolbox runtime check", fill=0, font=font)
d.text((150, 450), "The quick brown fox jumps", fill=0, font=font)
img.save(sys.argv[1], resolution=200)
PY
run "$RT/python/bin/python3" -m ocrmypdf -q --output-type pdfa --rasterizer pypdfium --pdfa-backend internal -O 2 "$WORK/in.pdf" "$WORK/out.pdf"
run "$RT/python/bin/python3" - "$WORK/out.pdf" <<'PY'
import sys
from pdfminer.high_level import extract_text
text = extract_text(sys.argv[1])
assert "quick brown fox" in text.lower(), f"OCR text missing: {text!r}"
print("OCR text found:", " ".join(text.split()))
PY
echo "Runtime OK"
