#Requires -Version 5.1
<#
.SYNOPSIS
  Builds the self-contained Windows OCR runtime (Python + ocrmypdf, Tesseract,
  pngquant) into src-tauri\resources\runtime, to be shipped inside the
  Tauri installer. Windows counterpart of bundle-runtime-macos.sh.

.DESCRIPTION
  Ghostscript is intentionally not included (AGPL): ocrmypdf rasterizes with
  pypdfium2 and builds PDF/A with pikepdf instead.

  Requirements (build machine only):
    - uv        (winget install astral-sh.uv)
    - 7-Zip     (winget install 7zip.7zip; preinstalled on GitHub runners)
    - Internet access (Tesseract installer, tessdata, pngquant)

  Run:  powershell -ExecutionPolicy Bypass -File scripts\bundle-runtime-windows.ps1

.PARAMETER Languages
  Tesseract languages to ship (default: eng osd). Also read from $env:TESS_LANGS.
#>
[CmdletBinding()]
param(
  [string]$PythonVersion = $(if ($env:PYTHON_VERSION) { $env:PYTHON_VERSION } else { '3.13' }),
  [string]$OcrmypdfVersion = $env:OCRMYPDF_VERSION,
  [string[]]$Languages = $(if ($env:TESS_LANGS) { $env:TESS_LANGS -split '\s+' } else { @('eng', 'osd') }),
  # UB Mannheim builds are the de-facto Windows Tesseract distribution.
  [string]$TesseractUrl = $(if ($env:TESSERACT_URL) { $env:TESSERACT_URL } else {
      'https://github.com/UB-Mannheim/tesseract/releases/download/v5.5.0.20241111/tesseract-ocr-w64-setup-5.5.0.20241111.exe' }),
  [string]$PngquantUrl = 'https://pngquant.org/pngquant-windows.zip'
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is 10x slower with the progress bar
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Root = Split-Path -Parent $PSScriptRoot
$Out = Join-Path $Root 'src-tauri\resources\runtime'
$Work = Join-Path ([IO.Path]::GetTempPath()) ("bpt-runtime-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $Work | Out-Null

function Need($cmd, $hint) {
  if (-not (Get-Command $cmd -ErrorAction SilentlyContinue)) { throw "$cmd is required: $hint" }
}
function Invoke-Checked {
  param([string]$Exe, [Parameter(ValueFromRemainingArguments)][string[]]$Rest)
  & $Exe @Rest
  if ($LASTEXITCODE -ne 0) { throw "$Exe $($Rest -join ' ') failed with exit code $LASTEXITCODE" }
}
function Find-7z {
  $c = Get-Command 7z -ErrorAction SilentlyContinue
  if ($c) { return $c.Source }
  foreach ($p in @("$env:ProgramFiles\7-Zip\7z.exe", "${env:ProgramFiles(x86)}\7-Zip\7z.exe")) {
    if (Test-Path $p) { return $p }
  }
  throw '7-Zip is required: winget install 7zip.7zip'
}

Need uv 'winget install astral-sh.uv'
$SevenZip = Find-7z

try {
  Write-Host "==> Cleaning $Out"
  if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
  New-Item -ItemType Directory -Force "$Out\bin", "$Out\tessdata" | Out-Null

  # ------------------------------------------------------------ Python
  Write-Host "==> Python $PythonVersion (python-build-standalone via uv)"
  Invoke-Checked uv python install $PythonVersion
  $PyExe = (& uv python find --managed-python $PythonVersion).Trim()
  if ($LASTEXITCODE -ne 0 -or -not (Test-Path $PyExe)) { throw "uv could not find a managed Python $PythonVersion" }
  $PyHome = Split-Path -Parent $PyExe
  Copy-Item -Recurse $PyHome "$Out\python"
  Remove-Item -Force -ErrorAction SilentlyContinue "$Out\python\Lib\EXTERNALLY-MANAGED"
  $Py = "$Out\python\python.exe"

  $spec = if ($OcrmypdfVersion) { "ocrmypdf==$OcrmypdfVersion" } else { 'ocrmypdf' }
  Write-Host "==> Installing $spec"
  Invoke-Checked uv pip install --quiet --python $Py --no-cache $spec

  Write-Host '==> Pruning Python'
  foreach ($d in 'Lib\test', 'Lib\idlelib', 'Lib\tkinter', 'Lib\turtledemo', 'Lib\ensurepip',
                 'Lib\lib2to3', 'tcl', 'include', 'libs', 'Doc', 'Scripts') {
    $p = Join-Path "$Out\python" $d
    if (Test-Path $p) { Remove-Item -Recurse -Force $p }
  }
  Get-ChildItem "$Out\python\DLLs" -Include '_tkinter*', 'tcl*', 'tk*' -Recurse -ErrorAction SilentlyContinue |
    Remove-Item -Force
  Get-ChildItem "$Out\python" -Recurse -Directory -Filter '__pycache__' | Remove-Item -Recurse -Force
  Get-ChildItem "$Out\python\Lib\site-packages" -Recurse -Directory -Filter 'tests' |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue

  # Precompile with hash-based (not mtime-based) pycs: the installer rewrites
  # timestamps, and workers never write bytecode themselves.
  Write-Host '==> Precompiling bytecode'
  Invoke-Checked $Py -I -m compileall -q -j 0 --invalidation-mode unchecked-hash "$Out\python\Lib"

  # ------------------------------------------------------------ Tesseract
  Write-Host "==> Tesseract ($TesseractUrl)"
  $TessSetup = Join-Path $Work 'tesseract-setup.exe'
  Invoke-WebRequest -UseBasicParsing -Uri $TesseractUrl -OutFile $TessSetup
  $TessDir = Join-Path $Work 'tesseract'
  # The NSIS installer is extracted rather than run: no admin rights, no
  # registry entries, nothing left behind on the build machine.
  Invoke-Checked $SevenZip x -y "-o$TessDir" $TessSetup | Out-Null
  $TessExe = Get-ChildItem $TessDir -Recurse -Filter 'tesseract.exe' | Select-Object -First 1
  if (-not $TessExe) { throw 'tesseract.exe not found in the installer' }
  Copy-Item $TessExe.FullName "$Out\bin\"
  Get-ChildItem $TessExe.DirectoryName -Filter '*.dll' | Copy-Item -Destination "$Out\bin\"

  $TessData = Get-ChildItem $TessDir -Recurse -Directory -Filter 'tessdata' | Select-Object -First 1
  if ($TessData) {
    foreach ($item in 'configs', 'tessconfigs', 'pdf.ttf') {
      $p = Join-Path $TessData.FullName $item
      if (Test-Path $p) { Copy-Item -Recurse $p "$Out\tessdata\" }
    }
  }
  if (-not (Test-Path "$Out\tessdata\pdf.ttf")) {
    Invoke-WebRequest -UseBasicParsing -OutFile "$Out\tessdata\pdf.ttf" `
      -Uri 'https://github.com/tesseract-ocr/tesseract/raw/main/tessdata/pdf.ttf'
  }

  Write-Host "==> Tesseract data: $($Languages -join ' ')"
  foreach ($lang in $Languages) {
    Invoke-WebRequest -UseBasicParsing -OutFile "$Out\tessdata\$lang.traineddata" `
      -Uri "https://github.com/tesseract-ocr/tessdata_fast/raw/main/$lang.traineddata"
  }

  # ------------------------------------------------------------ pngquant
  # Only used by "Lossy"/"Aggressive" optimization; ocrmypdf skips it if absent.
  Write-Host '==> pngquant'
  try {
    $Zip = Join-Path $Work 'pngquant.zip'
    Invoke-WebRequest -UseBasicParsing -Uri $PngquantUrl -OutFile $Zip
    Expand-Archive -Force $Zip (Join-Path $Work 'pngquant')
    $pq = Get-ChildItem (Join-Path $Work 'pngquant') -Recurse -Filter 'pngquant.exe' | Select-Object -First 1
    Copy-Item $pq.FullName "$Out\bin\"
  } catch {
    Write-Warning "pngquant not bundled ($_). Lossy optimization will be skipped by ocrmypdf."
  }

  # ------------------------------------------------------------ Verify
  Write-Host '==> Verifying runtime with a clean environment'
  & (Join-Path $PSScriptRoot 'verify-runtime-windows.ps1') -Runtime $Out
  if ($LASTEXITCODE -ne 0) { throw 'runtime verification failed' }

  $size = (Get-ChildItem $Out -Recurse -File | Measure-Object -Sum Length).Sum / 1MB
  Write-Host ("Runtime ready at {0} ({1:N0} MB)" -f $Out, $size)
} finally {
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Work
}
