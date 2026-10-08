<#
.SYNOPSIS
    Regenerate bench/baseline.json: the ARGONE Task B baseline, one command.

.DESCRIPTION
    Runs the three existing harnesses and assembles their machine-readable
    outputs into bench/baseline.json:
      bench/compiler/measure.ps1  (compiler cost: check/build/split/peaks/sizes)
      bench/run.ps1               (5 NX-vs-Rust benchmarks)
      bench/workloads/run-workloads.ps1  (11 coverage workloads vs Rust)

    baseline.json records machine, toolchain, build modes, warm-up policy,
    sample counts and variability, so a future number is comparable or it
    says why it is not. Regenerating is one command from the repo root:

        powershell -File bench/baseline.ps1

    Full run takes roughly 20-35 minutes on the reference box, dominated
    by the compiler harness (11 programs x 7 modes x warm-up + Runs).

    Idle-machine protocol (the "noise characterised on an idle machine"
    gate item): close other applications, AC power, nothing else running.
    The compiler harness additionally interleaves modes within each repeat
    and drops repeats whose floor probe exceeds 1.5x the quietest floor
    (see bench/compiler/README.md); medians are reported everywhere, with
    min/max/sd/cv% alongside so min-vs-median disagreement is visible.

.PARAMETER Runs
    Timed samples per (program, mode) everywhere. Recorded in the file.

.PARAMETER SkipRust
    Skip the Rust side (requires rustc otherwise). The Rust columns come
    out null and the file says so; only for environments without rustc.
#>
[CmdletBinding()]
param(
    [int]$Runs = 5,
    [switch]$SkipRust
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$bench = $PSScriptRoot
$compilerDir = Join-Path $bench 'compiler'
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) 'nxbaseline'
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
New-Item -ItemType Directory -Force -Path $tmp | Out-Null

function Invoke-Step([string]$label, [scriptblock]$body) {
    Write-Host ""
    Write-Host "=== $label ===" -ForegroundColor Cyan
    & $body
}

# ---------------------------------------------------------------------------
# 0. Preconditions: corpus intact, toolchains present, nx builds.
# ---------------------------------------------------------------------------
Invoke-Step 'preconditions' {
    & (Join-Path $compilerDir 'gen.ps1') -Verify
    if ($LASTEXITCODE -ne 0) { throw 'generator corpus does not match manifest.json' }
    if (-not $SkipRust) {
        $null = Get-Command rustc -ErrorAction Stop
        $null = Get-Command clang -ErrorAction Stop
    }
    Push-Location $root
    # Cargo writes "Finished/Compiling" progress to stderr, which
    # PowerShell surfaces as error records; under the script-wide
    # 'Stop' preference that would terminate the run on a successful
    # build. Scope 'Continue' around the invocations (as run.ps1 does
    # globally) and judge by exit code, the reliable signal.
    $prevEAP = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & cargo build --release -p nx-driver 2>&1 | Out-Null
    $releaseCode = $LASTEXITCODE
    & cargo build -p nx-driver --offline 2>&1 | Out-Null
    $debugCode = $LASTEXITCODE
    $ErrorActionPreference = $prevEAP
    if ($releaseCode -ne 0) { throw 'release nx build failed' }
    if ($debugCode -ne 0) { throw 'debug nx build failed' }
    Pop-Location
}

$nxRelease = Join-Path $root 'target\release\nx.exe'
$nxVersion = (& $nxRelease --version) -join ''
$gitSha = (git -C $root rev-parse HEAD) -join ''
$clangVer = ((& clang --version) | Select-Object -First 1) -join ''
$rustVer = if (-not $SkipRust) { ((& rustc --version) -join '') } else { $null }
$cpu = Get-CimInstance Win32_Processor | Select-Object -First 1
$os = Get-CimInstance Win32_OperatingSystem
$ramGb = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
$machine = [ordered]@{
    cpu      = $cpu.Name.Trim()
    cores    = [int]$cpu.NumberOfLogicalProcessors
    ram_gb   = $ramGb
    os       = "$($os.Caption.Trim()) $($os.Version)"
}

# ---------------------------------------------------------------------------
# 1. Compiler cost (check / build / codegen-clang-link split / peaks / sizes).
# ---------------------------------------------------------------------------
Invoke-Step 'compiler cost' {
    & (Join-Path $compilerDir 'measure.ps1') -Runs $Runs -Tag baseline
}
$compSummary = Import-Csv (Join-Path $compilerDir 'results\compiler-baseline-summary.csv')
$compDerived = Import-Csv (Join-Path $compilerDir 'results\compiler-baseline-derived.csv')
$compContext = Get-Content (Join-Path $compilerDir 'results\compiler-baseline-context.json') -Raw |
    ConvertFrom-Json
$manifest = Get-Content (Join-Path $compilerDir 'programs\manifest.json') -Raw |
    ConvertFrom-Json
if ($manifest.Count -ne 11) { throw "expected the 11-program corpus, manifest lists $($manifest.Count)" }

# ---------------------------------------------------------------------------
# 2. Programs: the 5 benchmarks plus the 11 coverage workloads, vs Rust.
# ---------------------------------------------------------------------------
Invoke-Step 'NX-vs-Rust benchmarks' {
    $runArgs = @{ Runs = $Runs; EmitJson = (Join-Path $tmp 'run.json') }
    if ($SkipRust) { $runArgs['SkipRust'] = $true }
    & (Join-Path $bench 'run.ps1') @runArgs
}
Invoke-Step 'coverage workloads' {
    $wlArgs = @{ Runs = $Runs; EmitJson = (Join-Path $tmp 'workloads.json') }
    if ($SkipRust) { $wlArgs['SkipRust'] = $true }
    & (Join-Path $bench 'workloads\run-workloads.ps1') @wlArgs
}
$benchJson = Get-Content (Join-Path $tmp 'run.json') -Raw | ConvertFrom-Json
$workloadsJson = Get-Content (Join-Path $tmp 'workloads.json') -Raw | ConvertFrom-Json

# Workload -> gate category. Unknown names fail the self-check below so
# a new workload cannot slip in uncategorized.
$categories = @{
    fib = 'recursion/call overhead'; mandel = 'float loop'; matmul = 'numeric loop';
    listsum = 'list indexing'; dot = 'list indexing';
    strbuild = 'string construction'; strscan = 'string scanning';
    dictops = 'dict operations'; sortint = 'sorting'; sortstr = 'sorting';
    sieve = 'integer operators'; flowctl = 'control flow'; recursion = 'recursion depth';
    churn = 'allocation churn'; records = 'records/methods'; textstat = 'application-shaped'
}
$programs = @()
foreach ($row in @($benchJson.results) + @($workloadsJson.results)) {
    $suite = if ($benchJson.results -contains $row) { 'bench' } else { 'workloads' }
    if (-not $categories.ContainsKey($row.Name)) {
        throw "workload '$($row.Name)' has no gate category in baseline.ps1; add one"
    }
    $programs += [ordered]@{
        suite    = $suite
        name     = $row.Name
        category = $categories[$row.Name]
        status   = if ($row.Status) { $row.Status } else { 'ok' }
        nx_s     = $row.Nx
        rust_s   = $row.Rust
        nx_boxed_s = $row.NxBoxed
    }
}
if (($programs | Where-Object { $_.status -eq 'ok' }).Count -lt 16) {
    throw 'expected 5 benchmarks + 11 workloads to report ok'
}

# ---------------------------------------------------------------------------
# 3. Noise: floors from the compiler harness plus CV ranges everywhere.
# ---------------------------------------------------------------------------
$cvs = @($compSummary | ForEach-Object { [double]$_.cv_pct })
$noise = [ordered]@{
    protocol = 'Idle machine: no other user load, AC power. Compiler modes interleaved per repeat; repeats whose floor probe exceeds 1.5x the quietest floor are dropped (counts in n_dropped). Median reported everywhere; min alongside so disagreement is visible.'
    floor_probes_ms = @($compContext.floors)
    repeat_floor_ms = @($compContext.repeat_floor_ms)
    clean_floor_ms = @($compContext.clean_floor_ms)
    load_factor = $compContext.load_factor
    compiler_cv_pct = [ordered]@{
        min = ($cvs | Measure-Object -Minimum).Minimum
        max = ($cvs | Measure-Object -Maximum).Maximum
    }
}

# ---------------------------------------------------------------------------
# 4. Coverage: what is measured, and what is explicitly not (with reasons).
# ---------------------------------------------------------------------------
$coverage = [ordered]@{
    compiler_check_time            = 'measure.ps1 check mode per synthetic program'
    compiler_build_time            = 'measure.ps1 build mode per synthetic program'
    compiler_codegen_clang_split   = 'measure.ps1 emit-ir vs clang-O2/clangc-O2, subtraction and direct (must agree)'
    compiler_peak_working_set      = 'measure.ps1 peak_nx_mb + peak_llvm_mb (linker child invisible; lower bound)'
    compiler_output_size           = 'measure.ps1 .ll and .exe bytes per program'
    compiler_scaling               = 'funcs/stmts/flow ladders in programs/manifest.json'
    program_categories             = 'one row per category in the programs table above'
    not_covered_lexer_parser_checker_individually = 'CLI prints (--lex/--parse dump AST); no print-nothing mode exists. Bounded by check/dump-ir instead.'
    not_covered_per_module_cost    = 'corpus is single-module; HashMap-keyed loader/mem-plan scaling untested.'
    not_covered_linker_peak        = 'linker is a child of clang; outside both working sets.'
    not_covered_incremental_builds = 'harness defeats up_to_date() deliberately; warm builds unmeasured.'
    not_covered_nounbox_differential = 'representation question, covered by verify.ps1/CI rather than timings.'
}

$baseline = [ordered]@{
    generated_utc = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    nx_version    = $nxVersion
    git_sha       = $gitSha
    rust_skipped  = [bool]$SkipRust
    machine       = $machine
    toolchain     = [ordered]@{ clang = $clangVer; rustc = $rustVer }
    build_modes   = [ordered]@{
        compiler_profile        = 'release (compiler timings)'
        program_emitter_profile = 'debug (program runtimes are unaffected; only IR emission uses it)'
    }
    methodology = [ordered]@{
        runs_per_program_mode = $Runs
        warmup                = '1 untimed pass per (program, mode) before sample 1 (compiler harness)'
        statistic             = 'median; min/max/sd/cv alongside'
        correctness_gate      = 'outputs must match (NX vs Rust; rebuilt determinism for synthetics), else timings discarded'
    }
    compiler = [ordered]@{
        summary = @($compSummary)
        derived = @($compDerived)
        context = $compContext
    }
    programs = @($programs)
    noise    = $noise
    coverage = $coverage
}

$out = Join-Path $bench 'baseline.json'
$baseline | ConvertTo-Json -Depth 8 | Set-Content -Path $out -Encoding utf8NoBOM

# Self-check: the file must parse and contain every section with rows.
$check = Get-Content $out -Raw | ConvertFrom-Json
foreach ($k in @('generated_utc', 'machine', 'toolchain', 'methodology', 'compiler', 'programs', 'noise', 'coverage')) {
    if (-not $check.PSObject.Properties[$k]) { throw "baseline.json missing section '$k'" }
}
if ($check.programs.Count -lt 16) { throw "baseline.json programs table has $($check.programs.Count) rows, want >= 16" }
if ($check.compiler.summary.Count -ne 77) {
    throw "baseline.json compiler summary has $($check.compiler.summary.Count) rows, want 77 (11 programs x 7 modes)"
}
Write-Host ""
Write-Host "baseline.json written: $($check.programs.Count) program rows, " -NoNewline
Write-Host "$($check.compiler.summary.Count) compiler rows." -ForegroundColor Green
