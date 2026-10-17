# Benchmark harness (Phase 2 baseline).
#
# Usage:  powershell -ExecutionPolicy Bypass -File benchmarks\run.ps1
#
# Requires `nx` and `clang` on PATH (or `cargo run -p nexum-driver --` instead of nx).
# Builds each benchmark with clang -O2, runs it N times, reports min/median.
param(
    [int]$Runs = 7,
    [string]$Nx = "nx"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $env:TEMP "nexum_bench"
New-Item -ItemType Directory -Force -Path $work | Out-Null

$results = @()

foreach ($bench in Get-ChildItem (Join-Path $PSScriptRoot "*.nx")) {
    $name = $bench.BaseName

    # Compile to LLVM IR via nx
    $ll = Join-Path $work "$name.ll"
    & $Nx emit-ir $bench.FullName | Out-File -Encoding ascii $ll

    # Build with -O2
    $exe = Join-Path $work "$name.exe"
    & clang -Wno-override-module -O2 $ll -o $exe

    # Warmup
    & $exe | Out-Null

    $times = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        & $exe | Out-Null
        $sw.Stop()
        $times += $sw.Elapsed.TotalMilliseconds
    }

    $sorted = $times | Sort-Object
    $min = $sorted[0]
    $med = $sorted[[int]($sorted.Count / 2)]
    $results += [pscustomobject]@{
        benchmark = $name
        runs      = $Runs
        min_ms    = [math]::Round($min, 2)
        median_ms = [math]::Round($med, 2)
    }
}

$results | Format-Table -AutoSize

$csv = Join-Path $PSScriptRoot "results.csv"
$first = -not (Test-Path $csv)
$results | ForEach-Object {
    [pscustomobject]@{
        timestamp = (Get-Date -Format "o")
        benchmark = $_.benchmark
        runs      = $_.runs
        min_ms    = $_.min_ms
        median_ms = $_.median_ms
    }
} | Export-Csv -Path $csv -Append -NoTypeInformation -Encoding ascii

Write-Output "appended to $csv (first=$first)"
