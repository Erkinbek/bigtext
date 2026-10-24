# Runs the core benchmark and the scripted GUI benchmark on the generated corpus.
param([string]$Dir = "testdata")
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release --bins
New-Item -ItemType Directory -Force target\bench | Out-Null
foreach ($f in "random-lines-1g.txt","random-lines-10g.txt","single-line-2g.txt","short-lines-100m.txt","utf16le.txt","cp1251.txt") {
  $p = Join-Path $Dir $f
  if (Test-Path $p) {
    Write-Host "=== $f"
    .\target\release\bigtext-bench.exe $p
    Start-Process -Wait .\target\release\bigtext.exe -ArgumentList "--bench-ui", $p, "--report", "target\bench\gui-$f.txt"
    Get-Content "target\bench\gui-$f.txt"
  }
}
