<#
.SYNOPSIS
    Compare Nexum against Rust on the same algorithms.

.DESCRIPTION
    Every benchmark is a pair of sources that must print identical output.
    Correctness is the gate: a benchmark that disagrees is reported as a
    failure and its timings are discarded, because a fast wrong answer is
    worth nothing.

    Both toolchains are measured the same way. NX emits LLVM IR and hands
    it to clang -O2; Rust is built with rustc -O on a single dependency-free
    crate. The comparison is compiler against compiler.

.PARAMETER Runs
    Timing repetitions per benchmark. The median is reported, which is
    the right statistic when a background process can steal a timeslice.

.PARAMETER SkipRust
    Run only the NX side. Useful when rustc is not installed.

.PARAMETER NoUnbox
    Also build each NX benchmark with NX_NOUNBOX=1 to show the cost of
    the all-boxed code path.
#>
[CmdletBinding()]
param(
    [int]$Runs = 5,
    [switch]$SkipRust,
    [switch]$NoUnbox
)

# Native tools (clang, rustc) write progress and warnings to stderr, which
# PowerShell surfaces as error records. Checking exit codes explicitly is
# the reliable way to judge a native build, so do not stop on stderr.
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$bench = $PSScriptRoot
$outDir = Join-Path $env:TEMP "nxbench"
if (Test-Path $outDir) { Remove-Item $outDir -Recurse -Force }
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

# Median of a set of seconds. Robust to a single scheduling hiccup.
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

# Build nx itself once; every benchmark then reuses it.
Write-Host "building nx..." -ForegroundColor DarkGray
Push-Location $root
& cargo build -q -p nx-driver --offline 2>&1 | Out-Null
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "nx build failed" }
$nxDriver = (Get-ChildItem "$root\target\debug\nx.exe" -ErrorAction SilentlyContinue)
Pop-Location
if (-not $nxDriver) { throw "nx.exe not found" }
$nx = $nxDriver.FullName

$names = @('fib', 'mandel', 'matmul', 'listsum')
$results = @()

foreach ($name in $names) {
    $nxSrc = Join-Path $bench "$name.nx"
    $rsSrc = Join-Path $bench "$name.rs"
    if (-not (Test-Path $nxSrc)) { continue }

    Write-Host ""
    Write-Host "=== $name ===" -ForegroundColor Cyan

    $nxExe = Join-Path $outDir "$name-nx.exe"
    # clang warns about the target triple on every build; that is expected
    # and would otherwise drown the benchmark output.
    & $nx build $nxSrc -o $nxExe 2>&1 | Where-Object { $_ -notmatch 'overriding the module target triple' } | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path $nxExe)) {
        Write-Host "  NX BUILD FAILED" -ForegroundColor Red
        $results += [pscustomobject]@{ Name = $name; Status = 'nx build failed' }
        continue
    }
    $nxOut = (& $nxExe) -join "`n"

    $rsExe = $null
    $rsOut = $null
    if (-not $SkipRust -and (Test-Path $rsSrc)) {
        $rsExe = Join-Path $outDir "$name-rs.exe"
        & rustc -O --edition 2021 -o $rsExe $rsSrc 2>&1 | Out-Null
        if ($LASTEXITCODE -ne 0 -or -not (Test-Path $rsExe)) {
            Write-Host "  RUST BUILD FAILED" -ForegroundColor Red
            $rsExe = $null
        } else {
            $rsOut = (& $rsExe) -join "`n"
        }
    }

    # Correctness gate.
    if ($rsOut -ne $null) {
        if ($nxOut -ceq $rsOut) {
            Write-Host "  correctness: MATCH" -ForegroundColor Green
        } else {
            Write-Host "  correctness: MISMATCH" -ForegroundColor Red
            Write-Host "    nx:  $($nxOut -replace "`n", ' | ')"
            Write-Host "    rs:  $($rsOut -replace "`n", ' | ')"
            $results += [pscustomobject]@{ Name = $name; Status = 'output mismatch' }
            continue
        }
    }

    $row = [ordered]@{ Name = $name; Nx = $null; Rust = $null; NxBoxed = $null }

    $row.Nx = Measure-Runs $nxExe $Runs
    Write-Host ("  nx:   {0,8:N3}s" -f $row.Nx) -ForegroundColor DarkGray

    if ($rsExe) {
        $row.Rust = Measure-Runs $rsExe $Runs
        Write-Host ("  rust: {0,8:N3}s" -f $row.Rust) -ForegroundColor DarkGray
    }

    if ($NoUnbox) {
        $bxExe = Join-Path $outDir "$name-nxbox.exe"
        $env:NX_NOUNBOX = '1'
        & $nx build $nxSrc -o $bxExe 2>&1 | Where-Object { $_ -notmatch 'overriding the module target triple' } | Out-Null
        Remove-Item Env:\NX_NOUNBOX
        if (Test-Path $bxExe) {
            $row.NxBoxed = Measure-Runs $bxExe $Runs
            Write-Host ("  nx (all boxed): {0,8:N3}s" -f $row.NxBoxed) -ForegroundColor DarkGray
        }
    }

    $results += [pscustomobject]$row
}

Write-Host ""
Write-Host "=== summary ===" -ForegroundColor Cyan
Write-Host ("{0,-10} {1,10} {2,10} {3,12} {4,10}" -f 'bench', 'nx', 'rust', 'rust/nx', 'nx boxed')
Write-Host ("-" * 56)
foreach ($r in $results) {
    if (-not $r.Nx) { Write-Host ("{0,-10} {1}" -f $r.Name, $r.Status) -ForegroundColor Red; continue }
    $ratio = if ($r.Rust) { "  {0:N2}x" -f ($r.Nx / $r.Rust) } else { "-" }
    $boxed = if ($r.NxBoxed) { "{0:N3}s" -f $r.NxBoxed } else { "-" }
    Write-Host ("{0,-10} {1,9:N3}s {2,9:N3}s {3,12} {4,10}" -f `
        $r.Name, $r.Nx, $(if ($r.Rust) { $r.Rust } else { 0 }), $ratio, $boxed)
}
Write-Host ""
Write-Host "rust/nx above 1.00 means Rust was faster." -ForegroundColor DarkGray

Remove-Item $outDir -Recurse -Force -ErrorAction SilentlyContinue
