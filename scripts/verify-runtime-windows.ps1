#Requires -Version 5.1
<#
.SYNOPSIS
  Runs the bundled Windows runtime with a scrubbed environment (only the
  runtime and System32 on PATH, no user site-packages), mirroring
  src-tauri/src/runtime.rs, and performs a real OCR end to end.
#>
[CmdletBinding()]
param([string]$Runtime = (Join-Path (Split-Path -Parent $PSScriptRoot) 'src-tauri\resources\runtime'))

$ErrorActionPreference = 'Stop'
$Runtime = (Resolve-Path $Runtime).Path
$Py = Join-Path $Runtime 'python\python.exe'
$Work = Join-Path ([IO.Path]::GetTempPath()) ("bpt-verify-" + [guid]::NewGuid())
New-Item -ItemType Directory -Force $Work | Out-Null

$saved = @{}
foreach ($k in 'PATH', 'PYTHONHOME', 'PYTHONPATH', 'PYTHONNOUSERSITE', 'PYTHONDONTWRITEBYTECODE',
               'PYTHONUTF8', 'TESSDATA_PREFIX', 'OMP_THREAD_LIMIT') {
  $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
}
function Set-Env($k, $v) { [Environment]::SetEnvironmentVariable($k, $v, 'Process') }
# Deliberately a simple function (no param block / [Parameter] attributes):
# an advanced function gets common parameters, so a native flag like
# `ocrmypdf -O 2` would bind as an ambiguous -OutVariable/-OutBuffer.
function Run {
  $Exe = $args[0]
  $Rest = @($args | Select-Object -Skip 1)
  & $Exe @Rest
  if ($LASTEXITCODE -ne 0) { throw "$Exe $($Rest -join ' ') failed with exit code $LASTEXITCODE" }
}

try {
  Set-Env PATH "$Runtime\bin;$Runtime\python;$env:SystemRoot\System32;$env:SystemRoot"
  Set-Env PYTHONHOME "$Runtime\python"
  Set-Env PYTHONPATH $null
  Set-Env PYTHONNOUSERSITE '1'
  Set-Env PYTHONDONTWRITEBYTECODE '1'
  Set-Env PYTHONUTF8 '1'
  Set-Env TESSDATA_PREFIX "$Runtime\tessdata"
  Set-Env OMP_THREAD_LIMIT '1'

  if (Get-ChildItem $Runtime -Recurse -Filter 'gswin*c.exe') { throw 'Ghostscript must not be bundled' }

  Write-Host '-- tesseract'; Run "$Runtime\bin\tesseract.exe" --version
  Run "$Runtime\bin\tesseract.exe" --list-langs
  Write-Host '-- pngquant'; Run "$Runtime\bin\pngquant.exe" --version
  Write-Host '-- ocrmypdf'; Run $Py -m ocrmypdf --version

  Write-Host '-- end-to-end OCR'
  $make = @'
import sys
from PIL import Image, ImageDraw, ImageFont
img = Image.new("L", (1700, 2200), 255)
d = ImageDraw.Draw(img)
font = ImageFont.load_default(size=64)
d.text((150, 300), "Bulk PDF Toolbox runtime check", fill=0, font=font)
d.text((150, 450), "The quick brown fox jumps", fill=0, font=font)
img.save(sys.argv[1], resolution=200)
'@
  # Scripts go through files: Windows PowerShell 5.1 mangles embedded quotes
  # in native-command arguments.
  Set-Content -Encoding UTF8 "$Work\make.py" $make
  Run $Py "$Work\make.py" "$Work\in.pdf"
  Run $Py -m ocrmypdf -q --output-type pdfa --rasterizer pypdfium --pdfa-backend internal -O 2 `
    "$Work\in.pdf" "$Work\out.pdf"
  $check = @'
import sys
from pdfminer.high_level import extract_text
text = extract_text(sys.argv[1])
assert "quick brown fox" in text.lower(), f"OCR text missing: {text!r}"
print("OCR text found:", " ".join(text.split()))
'@
  Set-Content -Encoding UTF8 "$Work\check.py" $check
  Run $Py "$Work\check.py" "$Work\out.pdf"
  Write-Host 'Runtime OK'
  $global:LASTEXITCODE = 0
} finally {
  foreach ($k in $saved.Keys) { Set-Env $k $saved[$k] }
  Remove-Item -Recurse -Force -ErrorAction SilentlyContinue $Work
}
