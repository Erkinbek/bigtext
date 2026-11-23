# Runs the benchmarks of BENCHMARKS.md on the generated corpus and keeps every report.
#   scripts\bench.ps1 [-Dir testdata] [-Runs 3] [-NoGui]
# Core numbers come from bigtext-bench (no window); GUI numbers from bigtext --bench-ui, which
# opens a window and takes the keyboard focus, so leave the PC alone while it runs.
# Reports go to target\bench\<date>\; random-lines-50g.txt is used when it exists
# (scripts\gen-testfiles.ps1 -Only random-lines-50g).
param([string]$Dir = "testdata", [int]$Runs = 3, [switch]$NoGui)
Set-Location (Join-Path $PSScriptRoot "..")
cargo build --release --bins
if ($LASTEXITCODE -ne 0) { exit 1 }
$out = Join-Path "target\bench" (Get-Date -Format "yyyy-MM-dd")
New-Item -ItemType Directory -Force $out | Out-Null
$core = "random-lines-1g.txt", "random-lines-10g.txt", "single-line-2g.txt", "short-lines-100m.txt", "utf16le.txt", "cp1251.txt"
$gui = "random-lines-1g.txt", "random-lines-10g.txt", "random-lines-50g.txt", "single-line-2g.txt"

function Invoke-Gui($file, $report, $extra) {
    $argv = @("--bench-ui", (Join-Path $Dir $file), "--report", (Join-Path $out $report)) + $extra
    Start-Process -Wait -FilePath .\target\release\bigtext.exe -ArgumentList $argv
}

foreach ($run in 1..$Runs) {
    foreach ($f in $core) {
        $p = Join-Path $Dir $f
        if (Test-Path $p) { .\target\release\bigtext-bench.exe $p > (Join-Path $out "core-$f-run$run.txt") }
    }
    $huge = Join-Path $Dir "random-lines-50g.txt"
    if (Test-Path $huge) {
        # No Replace All on 50 GB: it would write another 50 GB.
        .\target\release\bigtext-bench.exe $huge --no-replace > (Join-Path $out "core-random-lines-50g.txt-run$run.txt")
    }
    $one = Join-Path $Dir "random-lines-1g.txt"
    if (Test-Path $one) {
        .\target\release\bigtext-bench.exe $one --chunked --no-replace > (Join-Path $out "core-chunked-random-lines-1g.txt-run$run.txt")
    }
    if (-not $NoGui) {
        foreach ($f in $gui) {
            if (Test-Path (Join-Path $Dir $f)) { Invoke-Gui $f "gui-$f-run$run.txt" @() }
        }
    }
}
if (-not $NoGui) {
    foreach ($f in "random-lines-10g.txt", "random-lines-50g.txt") {
        if (Test-Path (Join-Path $Dir $f)) { Invoke-Gui $f "gui-search-$f.txt" @("--scenario", '"search:timeout connection"') }
    }
    $ten = Join-Path $Dir "random-lines-10g.txt"
    if (Test-Path $ten) {
        $before = (Get-Item $ten).Length
        Invoke-Gui "random-lines-10g.txt" "gui-close-search-random-lines-10g.txt" @("--scenario", "close-search")
        Invoke-Gui "random-lines-10g.txt" "gui-close-replace-random-lines-10g.txt" @("--scenario", "close-replace")
        if ((Get-Item $ten).Length -ne $before) { Write-Warning "close-replace changed $ten" }
    }
}
Get-ChildItem $out | ForEach-Object { "=== $($_.Name)"; Get-Content $_.FullName }
