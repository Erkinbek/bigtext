# Generates the benchmark corpus (~14 GB with the 10 GB file; -SkipBig for ~1.8 GB).
param([string]$Out = "testdata", [switch]$SkipBig, [switch]$Force)
Set-Location (Join-Path $PSScriptRoot "..")
$extra = @()
if ($SkipBig) { $extra += "--skip-big" }
if ($Force) { $extra += "--force" }
cargo run --release --bin gen-testfiles -- --out $Out @extra
