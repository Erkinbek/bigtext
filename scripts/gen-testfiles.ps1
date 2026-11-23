# Generates the benchmark corpus (~14 GB with the 10 GB file; -SkipBig for ~1.8 GB;
# -Only random-lines-50g adds the 50 GB file of BENCHMARKS.md).
param([string]$Out = "testdata", [switch]$SkipBig, [switch]$Force, [string]$Only = "")
Set-Location (Join-Path $PSScriptRoot "..")
$extra = @()
if ($SkipBig) { $extra += "--skip-big" }
if ($Force) { $extra += "--force" }
if ($Only) { $extra += "--only", $Only }
cargo run --release --bin gen-testfiles -- --out $Out @extra
