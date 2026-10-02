<#
.SYNOPSIS
    Run the coverage-audit workloads in this directory against Rust.

.DESCRIPTION
    Same convention as bench\run.ps1: every workload is a pair of sources
    that must print identical output, correctness is the gate, and a
    workload whose outputs disagree has its timings discarded.

    Both toolchains are built the same way run.ps1 builds them: NX through
    `nx build` (LLVM IR handed to clang -O2) and Rust through
    `rustc -O` on a single dependency-free crate. Nothing here is measured
    in debug.

.PARAMETER Runs
    Timing repetitions per workload. The median is reported.

.PARAMETER SkipRust
    Build and time the NX side only.

.PARAMETER Only
    Restrict the run to these workload names, comma separated.
#>
[CmdletBinding()]
param(
    [int]$Runs = 5,
    [switch]$SkipRust,
    [string]$Only = ''
)

$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent (Split-Path -Parent $PSScriptRoot)
$here = $PSScriptRoot
$outDir = Join-Path $env:TEMP "nxbench-workloads"
if (Test-Path $outDir) { Remove-Item $outDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

function Get-Median($values) {
    $s = $values | Sort-Object
    $n = $s.Count
    if ($n % 2 -eq 1) { return $s[[int]($n / 2)] }
    return ($s[$n / 2 - 1] + $s[$n / 2]) / 2
}

function Measure-Runs([string]$exe, [int]$runs) {
    $times = @()
    for ($i = 0; $i -lt $runs; $i++) {
        $t = (Measure-Command { & $exe | Out-Null }).TotalSeconds
        $times += $t
    }
    return (Get-Median $times)
}

Write-Host "building nx..." -ForegroundColor DarkGray
Push-Location $root
& cargo build -q -p nx-driver --offline 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "nx build failed" }
$nx = (Get-ChildItem "$root\target\debug\nx.exe").FullName
Pop-Location

$names = Get-ChildItem -Path $here -Filter '*.nx' |
    Sort-Object Name |
    ForEach-Object { $_.BaseName }
if ($Only) { $names = $names | Where-Object { $Only -split ',' -contains $_ } }

$results = @()

foreach ($name in $names) {
    $nxSrc = Join-Path $here "$name.nx"
    $rsSrc = Join-Path $here "$name.rs"
    if (-not (Test-Path $rsSrc)) { continue }

    Write-Host ""
    Write-Host "=== $name ===" -ForegroundColor Cyan

    $nxExe = Join-Path $outDir "$name-nx.exe"
    & $nx build $nxSrc -o $nxExe 2>&1 | Where-Object { $_ -notmatch 'overriding the module target triple' } | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $nxExe)) {
        Write-Host "  NX BUILD FAILED" -ForegroundColor Red
        $results += [pscustomobject]@{ Name = $name; Status = 'nx build failed' }
        continue
    }
    $nxOut = (& $nxExe) -join "`n"

    $rsExe = Join-Path $outDir "$name-rs.exe"
    & rustc -O --edition 2021 -o $rsExe $rsSrc 2>&1 | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $rsExe)) {
        Write-Host "  RUST BUILD FAILED" -ForegroundColor Red
        $results += [pscustomobject]@{ Name = $name; Status = 'rust build failed' }
        continue
    }
    $rsOut = (& $rsExe) -join "`n"

    if ($nxOut -ceq $rsOut) {
        Write-Host "  correctness: MATCH" -ForegroundColor Green
    } else {
        Write-Host "  correctness: MISMATCH" -ForegroundColor Red
        Write-Host "    nx:  $($nxOut -replace "`n", ' | ')"
        Write-Host "    rs:  $($rsOut -replace "`n", ' | ')"
        $results += [pscustomobject]@{ Name = $name; Status = 'output mismatch' }
        continue
    }

    if (-not $SkipRust) {
        $row = [ordered]@{ Name = $name; Nx = $null; Rust = $null }
        $row.Nx = Measure-Runs $nxExe $Runs
        $row.Rust = Measure-Runs $rsExe $Runs
        Write-Host ("  nx:   {0,8:N3}s" -f $row.Nx) -ForegroundColor DarkGray
        Write-Host ("  rust: {0,8:N3}s" -f $row.Rust) -ForegroundColor DarkGray
        $results += [pscustomobject]$row
    }
}

Write-Host ""
Write-Host "=== summary ===" -ForegroundColor Cyan
Write-Host ("{0,-12} {1,10} {2,10} {3,12}" -f 'workload', 'nx', 'rust', 'rust/nx')
Write-Host ("-" * 48)
foreach ($r in $results) {
    if (-not $r.Nx) { Write-Host ("{0,-12} {1}" -f $r.Name, $r.Status) -ForegroundColor Red; continue }
    $ratio = if ($r.Rust) { "  {0:N2}x" -f ($r.Nx / $r.Rust) } else { "-" }
    Write-Host ("{0,-12} {1,9:N3}s {2,9:N3}s {3,12}" -f $r.Name, $r.Nx, $(if ($r.Rust) { $r.Rust } else { 0 }), $ratio)
}
Write-Host ""
Write-Host "rust/nx above 1.00 means Rust was faster." -ForegroundColor DarkGray

Remove-Item $outDir -Recurse -Force -ErrorAction SilentlyContinue