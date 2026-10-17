# Phase 8: optimization comparison report.
# Compares fib30 (and others) at -O0 / -O2 / -O3 / -O3+LTO.
param(
    [int]$Runs = 5,
    [string]$Nx = "target\debug\nx.exe"
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$work = Join-Path $env:TEMP "nexum_phase8"
New-Item -ItemType Directory -Force -Path $work | Out-Null

$levels = @(
    @{ name = "-O0"; args = @("--opt-level", "0") ; clang = "-O0" },
    @{ name = "-O2"; args = @("--opt-level", "0") ; clang = "-O2" },
    @{ name = "-O3"; args = @("--opt-level", "1") ; clang = "-O3" },
    @{ name = "-O3+LTO"; args = @("--opt-level", "1") ; clang = "-O3" }
)

$results = @()
foreach ($bench in Get-ChildItem (Join-Path $PSScriptRoot "*.nx")) {
    $name = $bench.BaseName
    foreach ($lvl in $levels) {
        $ll = Join-Path $work "$name`_$($lvl.name).ll"
        & $Nx @($lvl.args) emit-ir $bench.FullName 2>$null | Out-File -Encoding ascii $ll
        $exe = Join-Path $work "$name`_$($lvl.name).exe"
        if ($lvl.name -eq "-O3+LTO") {
            & clang -Wno-override-module -O3 -flto=thin -fuse-ld=lld $ll -o $exe 2>$null
        } else {
            & clang -Wno-override-module $lvl.clang $ll -o $exe 2>$null
        }
        if (-not (Test-Path $exe)) { continue }
        & $exe | Out-Null
        $times = @()
        for ($i = 0; $i -lt $Runs; $i++) {
            $sw = [System.Diagnostics.Stopwatch]::StartNew()
            & $exe | Out-Null
            $sw.Stop()
            $times += $sw.Elapsed.TotalMilliseconds
        }
        $sorted = $times | Sort-Object
        $results += [pscustomobject]@{
            benchmark = $name
            level     = $lvl.name
            runs      = $Runs
            min_ms    = [math]::Round($sorted[0], 2)
            med_ms    = [math]::Round($sorted[[int]($sorted.Count / 2)], 2)
        }
    }
}

$md = "# Phase 8 optimization comparison`n`n"
$md += "Generated: $(Get-Date -Format 'yyyy-MM-dd HH:mm')  `n"
$md += "nx: $Nx  `n`n"
$md += "| benchmark | level | runs | min (ms) | median (ms) |`n"
$md += "| --------- | ----- | ---- | -------- | ----------- |`n"
foreach ($r in $results) {
    $md += "| $($r.benchmark) | $($r.level) | $($r.runs) | $($r.min_ms) | $($r.med_ms) |`n"
}
$md += "`nNotes:`n"
$md += "- MIR-level passes (const-fold, DCE, inlining) are on for -O3/-O3+LTO via --opt 1.`n"
$md += "- -O0/-O2 use --opt 0 (no MIR passes; clang optimizes).`n"
$md += '- `nx` is a debug build.' + "`n"
$out = Join-Path $PSScriptRoot "phase8_report.md"
[System.IO.File]::WriteAllText($out, $md)
Write-Output $md
Write-Output "written to $out"
