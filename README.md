# Bulk PDF Toolbox

A Tauri desktop app for macOS and Windows for batch PDF work. The first tool is
**Bulk OCR**: it makes hundreds of scanned PDFs searchable using
[ocrmypdf](https://ocrmypdf.readthedocs.io) and Tesseract. Both ship inside the
installer, so users don't need to install Homebrew, Python or anything else.

Ghostscript is **not** used. ocrmypdf 17 rasterizes pages with pdfium
(`--rasterizer pypdfium`) and builds PDF/A itself with pikepdf
(`--pdfa-backend internal`). This avoids shipping AGPL code. The trade-off:
with format **Auto**, a file whose PDF/A output can't be validated is saved as a
regular searchable PDF. With format **PDF/A**, that file is marked as failed.

## Features (Bulk OCR)

- Add files or whole folders (scanned recursively), or drag and drop them. Duplicates and non-PDFs are skipped.
- **Pause / Resume** freezes each worker's whole process tree (ocrmypdf + tesseract), so CPU use drops to zero right away and work continues from the same point on resume. On macOS this is SIGSTOP to the worker's process group. On Windows each worker runs in its own Job Object and every process in it is suspended (`NtSuspendProcess`).
- **Stop**: kills in-flight workers and puts those files back in the queue.
- **Cancel / Retry / Remove / Process next**, for single files or a multi-select (⌘/Shift-click).
- **Error handling**: ocrmypdf exit codes are mapped to clear states (encrypted, invalid PDF, already has text, missing input, output error, crashed, stalled). Temporary failures are retried automatically (configurable). If a language pack is missing, the whole queue pauses with a banner instead of failing every file.
- **Crash-safe output**: workers write to a hidden `.name.<id>.bpt-partial.pdf` file, which is renamed into place only after success. A file under the real output name is never truncated.
- **The queue survives quitting and crashes**: it is saved to `ocr-queue.json` in the app data folder (`~/Library/Application Support/com.bulkpdftoolbox.app/` or `%APPDATA%\com.bulkpdftoolbox.app\`). On the next launch it comes back paused, with a Resume prompt. Workers left over from a crash are cleaned up. On macOS a running worker kills itself when the app disappears, and a paused worker is reaped on the next launch. On Windows the Job Object is kill-on-close, so the OS ends the whole tree together with the app.
- **Stall timeout**: a file is killed if it makes no progress for N minutes of *unpaused* time. ocrmypdf's own tesseract timeout is wall-clock, so the worker sets it very high. Otherwise a long pause would silently drop the text layer from pages.
- Live per-file progress ("OCR · page 7/12") through a small ocrmypdf progress plugin, plus overall progress and an ETA.
- Output options: next to the original with a suffix, into a folder (keeping subfolders), or replace the original (the original goes to the Trash / Recycle Bin). File-exists policy: keep both, overwrite, or skip.
- Keeps the computer awake while processing (`caffeinate` / `SetThreadExecutionState`). Asks before quitting while work is running.

## Architecture

```
src/                         React UI (virtualized list, handles thousands of rows)
src-tauri/src/engine.rs      Queue state machine, scheduler, worker supervision, persistence
src-tauri/src/output.rs      Output path planning + atomic commit (unit tested)
src-tauri/src/proc.rs        Process-tree suspend/resume/kill (process groups / Job Objects), keep-awake
src-tauri/src/runtime.rs     Locates the bundled runtime and builds its environment
src-tauri/resources/worker/  Python worker: runs one ocrmypdf job, emits JSON-line events
src-tauri/resources/runtime/ (generated, per OS) Python + ocrmypdf, tesseract, pngquant
```

There is one worker process per file, so a crash, leak or hang only affects that file.
Parallelism is "files at once × threads per file". The defaults follow ocrmypdf's
advice of about √cores × √cores.

## Development

Prerequisites: Rust, Node + pnpm, and for building the runtime:

- macOS: `brew install tesseract pngquant dylibbundler uv`
- Windows: `winget install astral-sh.uv 7zip.7zip`, plus the MSVC build tools that Tauri needs

```bash
pnpm install
```

```bash
./scripts/bundle-runtime-macos.sh
```

On Windows, instead:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\bundle-runtime-windows.ps1
```

```bash
pnpm tauri dev
```

`bundle-runtime-macos.sh` copies the Homebrew binaries and rewrites their dylib
paths to be relative to the app. It installs ocrmypdf into a standalone Python
(python-build-standalone via uv), prunes the result, ad-hoc signs everything,
and then runs `verify-runtime-macos.sh`. That script performs a real OCR inside a
sandbox that **blocks /opt/homebrew**, which proves nothing leaks in from the
build machine.

`bundle-runtime-windows.ps1` does the same with uv's standalone Python, the
UB Mannheim Tesseract build (extracted with 7-Zip, not installed) and pngquant.
`verify-runtime-windows.ps1` then OCRs a test file with PATH reduced to the
runtime and System32.

Extra OCR languages: set `TESS_LANGS="eng osd deu fra hin guj"` before running either script.

UI-only work: `pnpm dev`, then open http://localhost:1420 in a browser. A
dev-only mock backend (`src/dev/mockBackend.ts`) fakes the engine.

### Tests

```bash
cd src-tauri && cargo test
```

This runs unit tests for output planning, plus end-to-end queue tests (`tests/queue.rs`) that run real ocrmypdf workers:
happy path, pause (asserts every process in the group is in state `T`, and that progress and
time freeze), resume, stop → requeue, cancel, encrypted/corrupt/already-OCRed/vanished inputs,
a missing language pausing the queue, persistence across a restart, and reaping workers
left stopped by a crash (Unix). On Windows the pause test checks that every thread of
each worker is in the `Suspended` state.

## Building the installers

Each installer has to be built on its own OS, because the bundled runtime is
native code.

```bash
CI=true pnpm tauri build
```

- **macOS** produces `src-tauri/target/release/bundle/macos/Bulk PDF Toolbox.app` and
  `bundle/dmg/Bulk PDF Toolbox_<ver>_aarch64.dmg`. `CI=true` skips the Finder
  AppleScript that only arranges the DMG window. Without it, the step fails when the
  terminal lacks Automation permission for Finder.
- **Windows** produces `src-tauri/target/release/bundle/nsis/Bulk PDF Toolbox_<ver>_x64-setup.exe`
  (settings in `src-tauri/tauri.windows.conf.json`). It is a per-user install, so no
  admin is needed. The installer fetches the WebView2 runtime if the PC lacks it.

**CI**: `.github/workflows/build.yml` builds both installers on GitHub's Windows
and Apple Silicon runners. It runs the end-to-end tests on each OS first, then
uploads the installers as workflow artifacts. Pushing a `v*` tag also attaches
them to a GitHub release.

### Before distributing publicly

- **Signing/notarization**: the bundled binaries are only ad-hoc signed. For a notarized build, sign every Mach-O under `Contents/Resources/runtime` with your Developer ID and the hardened runtime before signing the app. Python extension modules may need `com.apple.security.cs.disable-library-validation`.
- **Windows signing**: the `.exe` is unsigned, so SmartScreen shows "Windows protected your PC" until it is signed with a code-signing certificate (`bundle.windows.certificateThumbprint` or `signCommand`).
- **Licenses**: ocrmypdf is MPL-2.0, Tesseract is Apache-2.0, pdfium (via pypdfium2) is BSD/Apache, and pngquant is GPL-3.0. Include their notices. No AGPL components are shipped.
- **Architecture**: the runtime is built for the build machine's architecture: arm64 on macOS, x64 on Windows. Intel Macs and Windows on ARM need their own builds.
- **Icons**: still the Tauri defaults (`src-tauri/icons`).
