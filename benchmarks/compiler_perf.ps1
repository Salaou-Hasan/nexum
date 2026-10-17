# Compiler performance report generator (Phase 5).
#
# Usage:  powershell -ExecutionPolicy Bypass -File benchmarks\compiler_perf.ps1
#         [-Nx path\to\nx.exe] [-Runs N]
#
# Times `nx` compiling each example (front-end + MIR + codegen + IR emit)
# at opt level 0 and 1, and writes benchmarks\compiler_perf.md.
param(
    [int]$Runs = 5,
    [string]$Nx = "nx"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$examples = Join-Path $root "examples"

$rows = @()
foreach ($f in (Get-ChildItem $examples "*.nx" | Sort-Object Name)) {
    foreach ($opt in 0, 1) {
        $times = @()
        for ($i = 0; $i -lt $Runs; $i++) {
            $sw = [System.Diagnostics.Stopwatch]::StartNew()
            & $Nx emit-ir $f.FullName --opt-level $opt 2>&1 | Out-Null
            $sw.Stop()
            $times += $sw.Elapsed.TotalMilliseconds
        }
        $sorted = $times | Sort-Object
        $rows += [pscustomobject]@{
            example = $f.BaseName
            opt     = $opt
            runs    = $Runs
            min_ms  = [math]::Round($sorted[0], 2)
            med_ms  = [math]::Round($sorted[[int]($sorted.Count / 2)], 2)
        }
    }
}

$md = "# Compiler performance report (Phase 5)`n`n"
$md += "Generated: $(Get-Date -Format 'yyyy-MM-dd HH:mm')  `n"
$md += "nx: $Nx  `n`n"
$md += "| example | opt | runs | min (ms) | median (ms) |`n"
$md += "| ------- | --- | ---- | -------- | ----------- |`n"
foreach ($r in $rows) {
    $md += "| $($r.example) | $($r.opt) | $($r.runs) | $($r.min_ms) | $($r.med_ms) |`n"
}
$md += "`nNotes:`n"
$md += "- Compile time is front-end + MIR + codegen + IR text emission (no clang).`n"
$md += "- Opt level 1 runs const-fold, DCE and small-function inlining.`n"
$md += '- `nx` is a debug build; release numbers will differ.' + "`n"

$out = Join-Path $PSScriptRoot "compiler_perf.md"
[System.IO.File]::WriteAllText($out, $md)
Write-Output $md
Write-Output "written to $out"
