<#
.SYNOPSIS
    Compiler performance harness for Nexum: phase timings, peak memory, output size.

.DESCRIPTION
    Measures, per generated program, the wall-clock cost of each compiler
    mode, the peak working set of the compiler process, and the size of the
    artifact each mode produces. Runs are interleaved across modes within
    each repeat so that a background-load episode (this box is shared) cannot
    land entirely on one phase.

    Modes, in the order they run inside a repeat:

      check     nx check <f>                 frontend + type check only.
      dump-ir   nx dump-ir <f>               effect/summary analysis only.
      emit-ir   nx build <f> --emit-ir       check + codegen + IR serialisation.
                                              No clang. stdout is captured to a
                                              real file, which is what nx build
                                              does internally anyway, so this is
                                              the faithful "Nexum's own work"
                                              number rather than a pipe number.
      build     nx build <f> -o out.exe      the whole pipeline (forces a real
                                              rebuild by deleting the exe AND the
                                              .nxstamp first -- otherwise
                                              up_to_date() short-circuits and
                                              the measurement is meaningless).
      clang-O2  clang -O2 <ll> -o out.exe    clang alone, on exactly the IR nx
                                              produced. This is the cross-check
                                              on the subtraction method. NOTE it
                                              includes the NATIVE LINK, which on
                                              Windows is a large fixed cost that
                                              has nothing to do with IR quality.
      clangc-O2 clang -O2 -c <ll> -o out.obj compile only: LLVM parse + optimise
                                              + object emission, NO link. The
                                              gap clang-O2 minus clangc-O2 is the
                                              linker, and it turns out to be
                                              nearly constant.
      clang-O0  clang -O0 <ll> -o out.exe    same but unoptimised. The gap
                                              between O0 and O2 is how much of
                                              clang's time is the optimiser, i.e.
                                              how much is "irreducible backend"
                                              vs "clang spends its time
                                              re-optimising what we handed it".

    The build time therefore decomposes as

        build = emit_ir + clangc + link_and_driver_overhead
                    ^          ^         ^
                    |          |         link + `clang --version` probe +
                    |          |         the .ll write + up_to_date()
                    |          LLVM on the IR we produced
                    Nexum's own work: check + infer + mem plan +
                    effect analysis + codegen + IR serialisation

    and clang/Nexum is reported two independent ways:
      subtraction   clang_est    = med(build) - med(emit-ir)
      direct        clang_direct = med(clang-O2)
    They should agree; if they do not, the subtraction is not trustworthy and
    the report says so.

    Peak working set: a C# helper (NxProc, compiled with Add-Type at startup)
    samples Process.PeakWorkingSet64 in a 1 ms poll loop WHILE the child is
    alive. Reading it after WaitForExit() returns 0 on a handle PowerShell did
    not create -- that is the whole reason this helper exists instead of a
    Start-Process one-liner. nx spawns clang as a child process, so nx's peak
    does NOT include clang; the pipeline peak is the sum of the two columns
    and both are reported.

.PARAMETER Runs
    Timed samples per (program, mode). Warm-up runs are separate and untimed.

.PARAMETER ProgramsDir
    Directory of generated .nx files. Default .\programs (run gen.ps1 first).

.PARAMETER OutDir
    Where results land. Default .\results. All artifacts are written there,
    never next to the .nx sources.

.PARAMETER Only
    Wildcard filter on program base names, e.g. 'funcs-*'.

.EXAMPLE
    .\measure.ps1 -Runs 5
    .\measure.ps1 -Runs 3 -Only 'funcs-1600','funcs-3200' -SkipO0
#>
[CmdletBinding()]
param(
    [int]$Runs = 5,
    [int]$Warmup = 1,
    [string]$ProgramsDir = (Join-Path $PSScriptRoot 'programs'),
    [string]$OutDir = (Join-Path $PSScriptRoot 'results'),
    [string]$NxExe = 'C:\nexum\target\release\nx.exe',
    [string]$Clang = 'clang',
    [string[]]$Only,
    [switch]$SkipO0,
    [switch]$NoVerifyRun,
    [int]$PollMs = 1,
    [int]$TimeoutSec = 1800,
    [string]$Tag = (Get-Date -Format 'yyyyMMdd-HHmmss')
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Native runner. Compiled once per harness invocation.
#
# Why not Start-Process + $p.PeakWorkingSet64?
#   1. Start-Process -PassThru returns a Process whose handle PowerShell does
#      not retain, so PeakWorkingSet64 and ExitCode throw unless you touch
#      .Handle first -- fragile.
#   2. Start-Process costs ~7 ms of PowerShell overhead per launch, which is
#      the same order as the fastest thing we measure.
#   3. Redirecting stdout needs a drain thread either way; doing it in C#
#      keeps the data path off the PowerShell heap.
# ---------------------------------------------------------------------------
$RunnerSource = @'
using System; using System.Diagnostics; using System.IO; using System.Text; using System.Threading;

public sealed class NxRun {
    public double Seconds; public long PeakBytes; public int ExitCode;
    public long   StdoutBytes; public int Polls; public string Error;
    public string DrainError;
}

public static class NxProc {
    // CommandLineToArgvW quoting (.NET Framework has no ProcessStartInfo.ArgumentList).
    public static string Quote(string s) {
        if (s.Length > 0 && s.IndexOfAny(new char[]{' ', '\t', '"', '\n'}) < 0) return s;
        var sb = new StringBuilder(); sb.Append('"');
        for (int i = 0; i < s.Length; i++) {
            int b = 0;
            while (i < s.Length && s[i] == '\\') { b++; i++; }
            if (i == s.Length) { sb.Append('\\', b * 2); break; }
            if (s[i] == '"') { sb.Append('\\', b * 2 + 1); sb.Append('"'); }
            else { sb.Append('\\', b); sb.Append(s[i]); }
        }
        sb.Append('"'); return sb.ToString();
    }

    static string BuildArgs(string[] args) {
        var sb = new StringBuilder();
        for (int i = 0; i < args.Length; i++) { if (i > 0) sb.Append(' '); sb.Append(Quote(args[i])); }
        return sb.ToString();
    }

    // A null or empty stdoutPath means "discard". It MUST be tested with
    // IsNullOrEmpty, not == null: PowerShell binds $null to a C# string
    // parameter as String.Empty, so a `== null` test silently builds
    // FileStream("") -- which throws inside the drain thread, kills it, and
    // leaves the child blocked forever on a full stdout pipe. That is not
    // hypothetical: it made `nx dump-ir` appear to hang for 120 s when it
    // really takes 0.05 s. The drain failure is reported, not swallowed.
    public static NxRun Run(string file, string[] args, string stdoutPath, int pollMs, int timeoutMs) {
        var r = new NxRun();
        var psi = new ProcessStartInfo();
        psi.FileName = file; psi.Arguments = BuildArgs(args);
        psi.UseShellExecute = false; psi.CreateNoWindow = true;
        psi.RedirectStandardOutput = true; psi.RedirectStandardError = true; psi.RedirectStandardInput = true;

        // Open the sink BEFORE the child starts, so a bad path is a loud
        // error here rather than a silently dead drain thread.
        FileStream outFile = null;
        Stream dst = Stream.Null;
        if (!string.IsNullOrEmpty(stdoutPath)) {
            try {
                outFile = new FileStream(stdoutPath, FileMode.Create, FileAccess.Write, FileShare.Read, 1 << 16);
                dst = outFile;
            } catch (Exception ex) {
                r.Error = "sink: " + ex.Message; return r;
            }
        }

        var sw = Stopwatch.StartNew();
        Process p;
        try { p = Process.Start(psi); }
        catch (Exception ex) { r.Error = "start: " + ex.Message; r.Seconds = sw.Elapsed.TotalSeconds; return r; }

        long outBytes = 0;
        string drainErrMsg = null;
        var drainOut = new Thread(() => {
            var buf = new byte[1 << 16];
            while (true) {
                int c;
                try { c = p.StandardOutput.BaseStream.Read(buf, 0, buf.Length); }
                catch (Exception ex) { drainErrMsg = "read: " + ex.Message; break; }
                if (c <= 0) break;
                Interlocked.Add(ref outBytes, c);
                try { dst.Write(buf, 0, c); }
                catch (Exception ex) { drainErrMsg = "write: " + ex.Message; break; }
            }
        });
        var drainErr = new Thread(() => { try { p.StandardError.BaseStream.CopyTo(Stream.Null); } catch { } });
        drainOut.IsBackground = true; drainErr.IsBackground = true;
        drainOut.Start(); drainErr.Start();

        long peak = 0; int polls = 0; bool exited = false;
        // PeakWorkingSet64 must be sampled DURING the run. After the process
        // exits the value is not retrievable through this handle, which is
        // exactly the trap this loop exists to avoid.
        while (true) {
            if (p.WaitForExit(pollMs)) { exited = true; break; }
            polls++;
            try { p.Refresh(); long v = p.PeakWorkingSet64; if (v > peak) peak = v; } catch { }
            if (sw.ElapsedMilliseconds > timeoutMs) { try { p.Kill(); } catch { } r.Error = "timeout"; break; }
        }
        sw.Stop();
        try { p.Refresh(); long v = p.PeakWorkingSet64; if (v > peak) peak = v; } catch { }
        try { if (!exited) p.WaitForExit(); r.ExitCode = p.ExitCode; } catch { r.ExitCode = -1; }
        drainOut.Join(10000); drainErr.Join(10000);
        try { if (outFile != null) { outFile.Flush(); outFile.Dispose(); } } catch { }
        r.Seconds = sw.Elapsed.TotalSeconds;
        r.PeakBytes = peak; r.Polls = polls; r.StdoutBytes = outBytes;
        r.DrainError = drainErrMsg;
        if (drainErrMsg != null && r.Error == null) r.Error = "drain: " + drainErrMsg;
        try { p.Dispose(); } catch { }
        return r;
    }
}
'@

Write-Host "compiling runner..." -ForegroundColor DarkGray
Add-Type -TypeDefinition $RunnerSource -Language CSharp -ErrorAction Stop

# ---------------------------------------------------------------------------
# Environment / floor probes
# ---------------------------------------------------------------------------
function Get-ClangPath {
    $c = Get-Command $Clang -ErrorAction SilentlyContinue
    if ($c) { return $c.Source }
    foreach ($p in @('C:\Program Files\LLVM\bin\clang.exe', 'C:\Program Files (x86)\LLVM\bin\clang.exe')) {
        if (Test-Path $p) { return $p }
    }
    throw "clang not found (looked for '$Clang' and the standard LLVM install dirs)"
}

$clangExe = Get-ClangPath
if (-not (Test-Path $NxExe)) { throw "nx not found at '$NxExe'. Build it with: cargo build --release --offline -p nx-driver" }
$nxArgsVer = (& $NxExe --version) -join ''
$clangVerLine = (& $clangExe --version) | Select-Object -First 1

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$workRoot = Join-Path $OutDir "work-$Tag"
if (Test-Path $workRoot) { Remove-Item $workRoot -Recurse -Force }
New-Item -ItemType Directory -Force -Path $workRoot | Out-Null

Write-Host ""
Write-Host "nx      : $NxExe   ($nxArgsVer)" -ForegroundColor Cyan
Write-Host "clang   : $clangExe   ($clangVerLine)" -ForegroundColor Cyan

# --- machine context (recorded in the results so a rerun can be compared) ---
$cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1)
$os = (Get-CimInstance Win32_OperatingSystem)
$machine = [ordered]@{
    timestamp      = (Get-Date).ToString('o')
    tag            = $Tag
    cpu            = $cpu.Name.Trim()
    cores_logical  = [Environment]::ProcessorCount
    ram_gb         = [math]::Round($os.TotalVisibleMemorySize / 1MB, 2)
    os             = $os.Caption
    nx_exe         = $NxExe
    nx_version     = $nxArgsVer
    clang_exe      = $clangExe
    clang_version  = $clangVerLine
    runs           = $Runs
    warmup         = $Warmup
    poll_ms        = $PollMs
    ps_version     = $PSVersionTable.PSVersion.ToString()
}

# --- process-launch floor -------------------------------------------------
# On a shared box this is not noise, it is the measurement limit: any mode
# whose median is near these numbers cannot be resolved. Measured every run.
Write-Host "measuring process-launch floor..." -ForegroundColor DarkGray
function Measure-Floor([string]$label, [string]$exe, [string[]]$a, [int]$n = 15) {
    $s = @()
    for ($i = 0; $i -lt $n; $i++) { $r = [NxProc]::Run($exe, $a, $null, $PollMs, 120000); $s += $r.Seconds }
    $sorted = $s | Sort-Object
    $med = $sorted[[int](($sorted.Count - 1) / 2)]
    [pscustomobject]@{ label = $label; n = $n; min_ms = [math]::Round($sorted[0] * 1000, 2); med_ms = [math]::Round($med * 1000, 2); max_ms = [math]::Round($sorted[-1] * 1000, 2) }
}
$floorCmd = Measure-Floor 'cmd /c exit' $env:ComSpec @('/c', 'exit')
$floorNx = Measure-Floor 'nx --version' $NxExe @('--version')
$floorClang = Measure-Floor 'clang --version' $clangExe @('--version')
Write-Host ("  floor  cmd /c exit    {0,7:N2} ms (min {1:N2})" -f $floorCmd.med_ms, $floorCmd.min_ms)
Write-Host ("  floor  nx --version   {0,7:N2} ms (min {1:N2})" -f $floorNx.med_ms, $floorNx.min_ms)
Write-Host ("  floor  clang --version{0,7:N2} ms (min {1:N2})" -f $floorClang.med_ms, $floorClang.min_ms)
$machine['floor_cmd_ms'] = $floorCmd.med_ms
$machine['floor_nx_ms'] = $floorNx.med_ms
$machine['floor_clang_ms'] = $floorClang.med_ms

# ---------------------------------------------------------------------------
# Program discovery
# ---------------------------------------------------------------------------
if (-not (Test-Path $ProgramsDir)) { throw "no programs dir '$ProgramsDir' - run gen.ps1 first" }
$manifestPath = Join-Path $ProgramsDir 'manifest.json'
$manifest = @{}
if (Test-Path $manifestPath) {
    foreach ($m in (Get-Content $manifestPath -Raw | ConvertFrom-Json)) { $manifest[$m.file] = $m }
}
$programs = @(Get-ChildItem $ProgramsDir -Filter *.nx | Sort-Object Name)
if ($Only) { $programs = @($programs | Where-Object { $n = $_.BaseName; $Only | Where-Object { $n -like $_ } }) }
if ($programs.Count -eq 0) { throw "no programs selected" }

$modes = @('check', 'dump-ir', 'emit-ir', 'build', 'clangc-O2', 'clang-O2')
if (-not $SkipO0) { $modes += 'clang-O0' }

# ---------------------------------------------------------------------------
# Runner for one (program, mode)
# ---------------------------------------------------------------------------
$script:Failures = @()

function Invoke-Mode([string]$prog, [string]$mode, [string]$llPath, [string]$exePath, [string]$objPath, [string]$c2Exe, [string]$c0Exe, [string]$stampPath) {
    switch ($mode) {
        'check' {
            return [NxProc]::Run($NxExe, @('check', $prog), $null, $PollMs, $TimeoutSec * 1000)
        }
        'dump-ir' {
            return [NxProc]::Run($NxExe, @('dump-ir', $prog), $null, $PollMs, $TimeoutSec * 1000)
        }
        'emit-ir' {
            # stdout goes to a real file: this is the same string nx build
            # writes to its own temp .ll before handing it to clang, so this
            # is the faithful Nexum-side cost, not a pipe cost.
            return [NxProc]::Run($NxExe, @('build', $prog, '--emit-ir'), $llPath, $PollMs, $TimeoutSec * 1000)
        }
        'build' {
            # up_to_date() would short-circuit on a warm exe and report
            # "up to date" instantly. Both the exe and its .nxstamp must go,
            # or this measures a stat() call.
            Remove-Item $exePath -Force -ErrorAction SilentlyContinue
            Remove-Item $stampPath -Force -ErrorAction SilentlyContinue
            return [NxProc]::Run($NxExe, @('build', $prog, '-o', $exePath), $null, $PollMs, $TimeoutSec * 1000)
        }
        'clangc-O2' {
            # -c: LLVM only. No link, so this is the part of clang's cost that
            # is actually about the quality/shape of the IR nx handed it.
            return [NxProc]::Run($clangExe, @('-O2', '-c', $llPath, '-o', $objPath), $null, $PollMs, $TimeoutSec * 1000)
        }
        'clang-O2' {
            # Distinct output path: prog.exe must stay the artifact `nx build`
            # produced, otherwise whichever clang mode ran last would be
            # mistaken for the -O2 build output when its size is recorded.
            return [NxProc]::Run($clangExe, @('-O2', $llPath, '-o', $c2Exe), $null, $PollMs, $TimeoutSec * 1000)
        }
        'clang-O0' {
            return [NxProc]::Run($clangExe, @('-O0', $llPath, '-o', $c0Exe), $null, $PollMs, $TimeoutSec * 1000)
        }
    }
}

# ---------------------------------------------------------------------------
# Correctness gate: the corpus is only worth timing if what it compiles to is
# both sound and reproducible. There is no interpreter to use as a reference
# any more, so the gate is: the program builds, runs, exits clean, prints
# something, and two independent from-scratch builds of it agree byte for
# byte. Run once per program, untimed.
# ---------------------------------------------------------------------------
if (-not $NoVerifyRun) {
    Write-Host ""
    Write-Host "correctness gate (untimed)..." -ForegroundColor DarkGray
    foreach ($p in $programs) {
        $d = Join-Path $workRoot $p.BaseName
        New-Item -ItemType Directory -Force -Path $d | Out-Null
        $xe = Join-Path $d 'verify.exe'

        # (a) it must build at all
        $b = [NxProc]::Run($NxExe, @('build', $p.FullName, '-o', $xe), $null, $PollMs, $TimeoutSec * 1000)
        if ($b.ExitCode -ne 0 -or $b.Error) {
            Write-Host ("  {0,-16} BUILD FAILED {1}" -f $p.BaseName, $b.Error) -ForegroundColor Red
            $script:Failures += "$($p.BaseName): build failed"; continue
        }
        $nat = (& $xe 2>&1) -join "`n"
        if ([string]::IsNullOrWhiteSpace($nat)) {
            Write-Host ("  {0,-16} NO OUTPUT" -f $p.BaseName) -ForegroundColor Red
            $script:Failures += "$($p.BaseName): produced no output"; continue
        }

        # (b) codegen must be deterministic: build again from scratch and
        #     require byte-identical program output.
        $xe2 = Join-Path $d 'verify2.exe'
        $b2 = [NxProc]::Run($NxExe, @('build', $p.FullName, '-o', $xe2), $null, $PollMs, $TimeoutSec * 1000)
        if ($b2.ExitCode -ne 0 -or $b2.Error) {
            Write-Host ("  {0,-16} REBUILD FAILED {1}" -f $p.BaseName, $b2.Error) -ForegroundColor Red
            $script:Failures += "$($p.BaseName): rebuild failed"; continue
        }
        $nat2 = (& $xe2 2>&1) -join "`n"
        if ($nat.Trim() -cne $nat2.Trim()) {
            Write-Host ("  {0,-16} NON-DETERMINISTIC BUILD" -f $p.BaseName) -ForegroundColor Red
            $script:Failures += "$($p.BaseName): build not deterministic"; continue
        }
        Write-Host ("  {0,-16} OK  [{1}]" -f $p.BaseName, ($nat -replace "`n", ' ')) -ForegroundColor DarkGreen
    }
    if ($script:Failures.Count -gt 0) {
        Write-Host ""
        Write-Host "ABORT: $($script:Failures.Count) program(s) failed the correctness gate; timings discarded." -ForegroundColor Red
        $script:Failures | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
        exit 1
    }
}

# ---------------------------------------------------------------------------
# Timed passes
# ---------------------------------------------------------------------------
$raw = New-Object System.Collections.ArrayList
$t0 = Get-Date
foreach ($p in $programs) {
    $d = Join-Path $workRoot $p.BaseName
    New-Item -ItemType Directory -Force -Path $d | Out-Null
    $llPath = Join-Path $d 'prog.ll'
    $exePath = Join-Path $d 'prog.exe'
    $objPath = Join-Path $d 'prog.obj'
    $c2Exe = Join-Path $d 'clang-O2.exe'
    $c0Exe = Join-Path $d 'clang-O0.exe'
    $stampPath = "$exePath.nxstamp"

    Write-Host ""
    Write-Host ("=== {0}  ({1:N0} bytes) ===" -f $p.BaseName, $p.Length) -ForegroundColor Cyan

    # Warm-up: one untimed pass over every mode so the binary pages, the
    # source file and the temp dir are all in cache before sample 1.
    $wu = "  warmup "
    foreach ($m in $modes) {
        $r = Invoke-Mode $p.FullName $m $llPath $exePath $objPath $c2Exe $c0Exe $stampPath
        $wu += ("{0}={1:N3}s " -f $m, $r.Seconds)
        if ($r.Error) { $wu += "ERR($($r.Error)) " }
        if ($r.ExitCode -ne 0 -and $r.ExitCode -ne -1) { $wu += "EXIT$($r.ExitCode) " }
    }
    Write-Host $wu -ForegroundColor DarkGray

    $llBytes = 0; $exeBytes = 0
    if (Test-Path $llPath) { $llBytes = (Get-Item $llPath).Length }
    if (Test-Path $exePath) { $exeBytes = (Get-Item $exePath).Length }

    # Timed repeats. Modes are interleaved inside a repeat, NOT run in blocks:
    # a background-load episode on a shared box then perturbs all modes of one
    # repeat rather than poisoning one whole mode.
    #
    # Each repeat also carries its own floor probe. This box is shared with
    # other work, and load episodes are severe enough to inflate a single
    # mode by 3-6x (observed: nx dump-ir on funcs-800 read 0.098 s standalone
    # and 29.9 s inside a loaded repeat). A repeat whose floor is far above
    # the session minimum is dropped from the statistics -- explicitly and
    # reversibly, with the dropped rows still present in the raw CSV.
    for ($rep = 1; $rep -le $Runs; $rep++) {
        $fp = @()
        for ($k = 0; $k -lt 3; $k++) { $fp += ([NxProc]::Run($NxExe, @('--version'), $null, $PollMs, 120000)).Seconds }
        $repFloor = ($fp | Sort-Object)[0]
        $line = "  rep {0}/{1} floor={2:N1}ms " -f $rep, $Runs, ($repFloor * 1000)
        foreach ($m in $modes) {
            $r = Invoke-Mode $p.FullName $m $llPath $exePath $objPath $c2Exe $c0Exe $stampPath
            $null = $raw.Add([pscustomobject]@{
                    program    = $p.BaseName
                    mode       = $m
                    rep        = $rep
                    rep_floor_ms = [math]::Round($repFloor * 1000, 2)
                    seconds    = $r.Seconds
                    peak       = $r.PeakBytes
                    polls      = $r.Polls
                    exit       = $r.ExitCode
                    error      = $r.Error
                })
            $line += ("{0}={1:N3}s " -f $m, $r.Seconds)
        }
        Write-Host $line -ForegroundColor DarkGray
    }
    Write-Host ("  src={0:N0}B  ll={1:N0}B  exe={2:N0}B" -f $p.Length, $llBytes, $exeBytes) -ForegroundColor DarkGray
}
$wall = ((Get-Date) - $t0).TotalMinutes

# ---------------------------------------------------------------------------
# Summarise
# ---------------------------------------------------------------------------
function Get-Stats($vals) {
    $s = @($vals | Sort-Object)
    $n = $s.Count
    if ($n -eq 0) { return $null }
    $med = if ($n % 2 -eq 1) { $s[[int]($n / 2)] } else { ($s[$n / 2 - 1] + $s[$n / 2]) / 2 }
    $mean = ($s | Measure-Object -Average).Average
    $sum = 0.0
    foreach ($v in $s) { $sum += ($v - $mean) * ($v - $mean) }
    $sd = if ($n -gt 1) { [math]::Sqrt($sum / ($n - 1)) } else { 0 }
    [pscustomobject]@{
        n = $n; min = $s[0]; med = $med; max = $s[-1]; mean = $mean
        sd = $sd; cv = if ($mean -gt 0) { $sd / $mean } else { 0 }
        vals = ($s | ForEach-Object { [math]::Round($_, 4) }) -join ' '
    }
}

# A repeat is "clean" when its own floor probe is within $LoadFactor of the
# quietest repeat observed anywhere in the session. Load on this box is
# machine-wide, so one session-wide threshold is the right filter.
$LoadFactor = 1.5
$allFloors = @($raw | ForEach-Object { $_.rep_floor_ms } | Sort-Object -Unique)
$floorMin = if ($allFloors.Count) { $allFloors[0] } else { 0 }
$cleanFloors = @($allFloors | Where-Object { $_ -le $LoadFactor * $floorMin })
Write-Host ""
Write-Host ("repeat floors (ms, distinct): {0}" -f (($allFloors | ForEach-Object { '{0:N1}' -f $_ }) -join ', '))
Write-Host ("clean repeats: floor <= {0:N1}ms ({1} of {2} distinct floor readings kept)" -f ($LoadFactor * $floorMin), $cleanFloors.Count, $allFloors.Count)

$summary = @()
foreach ($p in $programs) {
    foreach ($m in $modes) {
        $all = @($raw | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq $m })
        if ($all.Count -eq 0) { continue }
        # A run that errored (timeout, drain failure, non-zero exit) is NOT a
        # measurement. Excluding it from the statistics matters: a single
        # 120 s timeout in the middle of seven 0.05 s runs would otherwise
        # become the median. They stay in the raw CSV and are counted here.
        $errored = @($all | Where-Object { $_.error -or $_.exit -ne 0 })
        $clean = @($all | Where-Object { -not $_.error -and $_.exit -eq 0 })
        $rows = @($clean | Where-Object { $_.rep_floor_ms -le $LoadFactor * $floorMin })
        $dropped = $clean.Count - $rows.Count
        if ($rows.Count -eq 0) { $rows = $clean }
        if ($rows.Count -eq 0) {
            Write-Host ("  {0}/{1}: NO VALID RUNS ({2} errored: {3})" -f $p.BaseName, $m, $errored.Count, (($errored | Select-Object -First 1).error)) -ForegroundColor Red
            $script:Failures += "$($p.BaseName)/$m : $($errored.Count) errored run(s)"
            continue
        }
        $st = Get-Stats $rows.seconds
        $pkRows = @($rows | Where-Object { $_.peak -gt 0 })
        $pkMax = if ($pkRows.Count) { ($pkRows | Measure-Object peak -Maximum).Maximum } else { 0 }
        $summary += [pscustomobject]@{
            program = $p.BaseName
            family = $(if ($manifest.ContainsKey("$($p.BaseName).nx")) { $manifest["$($p.BaseName).nx"].family } else { '' })
            axis = $(if ($manifest.ContainsKey("$($p.BaseName).nx")) { $manifest["$($p.BaseName).nx"].axis } else { '' })
            mode = $m
            n = $st.n
            n_dropped = $dropped
            n_error = $errored.Count
            med_s = [math]::Round($st.med, 4)
            min_s = [math]::Round($st.min, 4)
            max_s = [math]::Round($st.max, 4)
            mean_s = [math]::Round($st.mean, 4)
            sd_s = [math]::Round($st.sd, 4)
            cv_pct = [math]::Round($st.cv * 100, 1)
            peak_mb = [math]::Round($pkMax / 1MB, 2)
            peak_sampled = ($pkRows.Count -gt 0)
            samples_s = $st.vals
        }
    }
}

# Derived split
$derived = @()
foreach ($p in $programs) {
    function Med($m) {
        $r = $summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq $m }
        if ($r) { return $r.med_s } else { return $null }
    }
    $tCheck = Med 'check'; $tEmit = Med 'emit-ir'; $tBuild = Med 'build'
    $tC2 = Med 'clang-O2'; $tC0 = Med 'clang-O0'; $tDump = Med 'dump-ir'
    $tCc = Med 'clangc-O2'
    $d = Join-Path $workRoot $p.BaseName
    $llB = if (Test-Path (Join-Path $d 'prog.ll')) { (Get-Item (Join-Path $d 'prog.ll')).Length } else { 0 }
    $exeB = if (Test-Path (Join-Path $d 'prog.exe')) { (Get-Item (Join-Path $d 'prog.exe')).Length } else { 0 }
    $pkCheck = ($summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq 'check' }).peak_mb
    $pkEmit = ($summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq 'emit-ir' }).peak_mb
    $pkBuild = ($summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq 'build' }).peak_mb
    $pkC2 = ($summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq 'clang-O2' }).peak_mb
    $pkCc = ($summary | Where-Object { $_.program -eq $p.BaseName -and $_.mode -eq 'clangc-O2' }).peak_mb
    # clangc-O2 is the mode whose memory scales with IR size: clang -O2 -c holds the
    # whole optimised module, whereas clang -O2 <ll> -o exe spawns the linker as a
    # child process whose memory never appears in clang's own working set.
    $codegen = if ($null -ne $tEmit -and $null -ne $tCheck) { $tEmit - $tCheck } else { $null }
    $clangEst = if ($null -ne $tBuild -and $null -ne $tEmit) { $tBuild - $tEmit } else { $null }
    # Residual: what build costs that neither our own emit-ir nor clang's
    # compile step accounts for -- the .ll write, the `clang --version`
    # probe, the up_to_date() check, and the native LINK.
    $resid = if ($null -ne $tBuild -and $null -ne $tEmit -and $null -ne $tCc) { $tBuild - $tEmit - $tCc } else { $null }
    $linkOnly = if ($null -ne $tC2 -and $null -ne $tCc) { $tC2 - $tCc } else { $null }
    # Floor-subtracted: on this box a process launch costs ~17 ms (nx) /
    # ~39 ms (clang), which is not a rounding error for the small programs.
    $fNx = $floorNx.med_ms / 1000
    $fCl = $floorClang.med_ms / 1000
    $derived += [pscustomobject]@{
        program = $p.BaseName
        family = $(if ($manifest.ContainsKey("$($p.BaseName).nx")) { $manifest["$($p.BaseName).nx"].family } else { '' })
        axis = $(if ($manifest.ContainsKey("$($p.BaseName).nx")) { $manifest["$($p.BaseName).nx"].axis } else { '' })
        src_b = $p.Length
        ll_b = $llB
        exe_b = $exeB
        check_s = $tCheck
        dump_ir_s = $tDump
        emit_ir_s = $tEmit
        build_s = $tBuild
        codegen_est_s = $(if ($null -ne $codegen) { [math]::Round($codegen, 4) })
        clang_est_s = $(if ($null -ne $clangEst) { [math]::Round($clangEst, 4) })
        clangc_o2_s = $tCc
        clang_o2_direct_s = $tC2
        clang_o0_direct_s = $tC0
        link_only_s = $(if ($null -ne $linkOnly) { [math]::Round($linkOnly, 4) })
        link_and_driver_s = $(if ($null -ne $resid) { [math]::Round($resid, 4) })
        clang_o2_over_o0 = $(if ($tC2 -and $tC0) { [math]::Round($tC2 / $tC0, 2) })
        codegen_pct = $(if ($tBuild -and $null -ne $codegen) { [math]::Round(100 * $codegen / $tBuild, 1) })
        clang_pct = $(if ($tBuild -and $null -ne $clangEst) { [math]::Round(100 * $clangEst / $tBuild, 1) })
        # floor-subtracted
        emit_ir_fs_s = $(if ($null -ne $tEmit) { [math]::Round($tEmit - $fNx, 4) })
        clangc_fs_s = $(if ($null -ne $tCc) { [math]::Round($tCc - $fCl, 4) })
        peak_nx_mb = $pkEmit
        peak_llvm_mb = $pkCc
        peak_clang_link_mb = $pkC2
        peak_clang_mb = $pkCc
        peak_pipeline_mb = $(if ($null -ne $pkEmit -and $null -ne $pkCc) { [math]::Round($pkEmit + $pkCc, 2) })
    }
}

# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------
$rawCsv = Join-Path $OutDir "compiler-$Tag-raw.csv"
$sumCsv = Join-Path $OutDir "compiler-$Tag-summary.csv"
$derCsv = Join-Path $OutDir "compiler-$Tag-derived.csv"
$jsPath = Join-Path $OutDir "compiler-$Tag-context.json"
$txtPath = Join-Path $OutDir "compiler-$Tag-report.txt"

$raw | Export-Csv -Path $rawCsv -NoTypeInformation -Encoding UTF8
$summary | Export-Csv -Path $sumCsv -NoTypeInformation -Encoding UTF8
$derived | Export-Csv -Path $derCsv -NoTypeInformation -Encoding UTF8
($machine + [ordered]@{
    floors = @($floorCmd, $floorNx, $floorClang)
    load_factor = $LoadFactor
    repeat_floor_ms = $allFloors
    clean_floor_ms = $cleanFloors
    wall_minutes = [math]::Round($wall, 2)
}) | ConvertTo-Json -Depth 5 | Set-Content $jsPath -Encoding UTF8

$sb = New-Object System.Text.StringBuilder
function W($s) { $null = $sb.AppendLine($s) }
W "Nexum compiler performance measurement"
W "tag=$Tag   runs=$Runs  warmup=$Warmup  wall=${wall} min"
W "nx     : $NxExe ($nxArgsVer)"
W "clang  : $clangExe ($clangVerLine)"
W "machine: $($machine.cpu) / $($machine.cores_logical) logical cores / $($machine.ram_gb) GB"
W "ps     : $($machine.ps_version)"
W ""
W "process-launch floor (median of $($floorCmd.n)) -- nothing below this is resolvable:"
W ("  cmd /c exit     {0,7:N2} ms" -f $floorCmd.med_ms)
W ("  nx --version    {0,7:N2} ms" -f $floorNx.med_ms)
W ("  clang --version {0,7:N2} ms" -f $floorClang.med_ms)
W ""
W ("{0,-14} {1,9} {2,9} {3,9} {4,9} {5,9} {6,9} {7,9} {8,9} {9,9} {10,9}" -f 'program', 'check_s', 'emit_ir', 'codegen', 'clang-c', 'link+drv', 'build_s', 'll_KB', 'nxPK_MB', 'llvmPK_MB', 'exe_KB')
W ('-' * 122)
foreach ($r in $derived) {
    W ("{0,-14} {1,9:N3} {2,9:N3} {3,9:N3} {4,9:N3} {5,9:N3} {6,9:N3} {7,9:N0} {8,9:N1} {9,9:N1} {10,9:N0}" -f `
            $r.program, $r.check_s, $r.emit_ir_s, $r.codegen_est_s, $r.clangc_o2_s, `
            $r.link_and_driver_s, $r.build_s, ($r.ll_b / 1KB), $r.peak_nx_mb, $r.peak_clang_mb, ($r.exe_b / 1KB))
}
W ""
W "per-mode medians (s), with spread:"
W ("repeat floors observed (ms): {0}" -f (($allFloors | ForEach-Object { '{0:N1}' -f $_ }) -join ', '))
W ("clean-repeat filter: rep_floor <= {0:N1}ms ; {1} of {2} distinct readings kept" -f ($LoadFactor * $floorMin), $cleanFloors.Count, $allFloors.Count)
W ""
W ("{0,-14} {1,10} {2,4} {3,6} {4,6} {5,10} {6,9} {7,9} {8,9} {9,7} {10,7}" -f 'program', 'mode', 'n', 'drop', 'err', 'med', 'min', 'max', 'sd', 'cv%', 'pkMB')
foreach ($r in $summary) {
    W ("{0,-14} {1,10} {2,4} {3,6} {4,6} {5,10:N4} {6,9:N4} {7,9:N4} {8,9:N4} {9,7:N1} {10,7:N1}" -f $r.program, $r.mode, $r.n, $r.n_dropped, $r.n_error, $r.med_s, $r.min_s, $r.max_s, $r.sd_s, $r.cv_pct, $r.peak_mb)
}
if ($script:Failures.Count -gt 0) {
    W ""
    W "ERRORED (program, mode) pairs -- excluded from all statistics above:"
    $script:Failures | ForEach-Object { W "  $_" }
}
W ""
W "raw samples: $rawCsv"
W "summary    : $sumCsv"
W "derived    : $derCsv"
W "context    : $jsPath"
Set-Content -Path $txtPath -Value $sb.ToString() -Encoding UTF8

Write-Host ""
Write-Host ("{0,-14} {1,9} {2,9} {3,9} {4,9} {5,9} {6,9} {7,9} {8,9} {9,9} {10,9}" -f 'program', 'check_s', 'emit_ir', 'codegen', 'clang-c', 'link+drv', 'build_s', 'll_KB', 'nxPK_MB', 'llvmPK_MB', 'exe_KB') -ForegroundColor White
Write-Host ('-' * 122)
foreach ($r in $derived) {
    Write-Host ("{0,-14} {1,9:N3} {2,9:N3} {3,9:N3} {4,9:N3} {5,9:N3} {6,9:N3} {7,9:N0} {8,9:N1} {9,9:N1} {10,9:N0}" -f `
            $r.program, $r.check_s, $r.emit_ir_s, $r.codegen_est_s, $r.clangc_o2_s, `
            $r.link_and_driver_s, $r.build_s, ($r.ll_b / 1KB), $r.peak_nx_mb, $r.peak_clang_mb, ($r.exe_b / 1KB))
}
Write-Host ""
Write-Host "report: $txtPath" -ForegroundColor DarkGray
Write-Host "raw   : $rawCsv" -ForegroundColor DarkGray
