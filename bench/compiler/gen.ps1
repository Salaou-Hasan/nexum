<#
.SYNOPSIS
    Deterministic generator of synthetic Nexum programs for compiler benchmarking.

.DESCRIPTION
    Compiler cost does not scale with line count. It scales with how many
    distinct *shapes* the backend has to do work on: number of function
    symbols to declare, length of the SSA chain inside one function, number
    of loops and method calls to lower. So this generator offers three
    orthogonal families, each with one tunable axis, plus a constant fixed
    seed:

      funcs  N  ->  N independent top-level functions, STMT_PER_FN statements
                    each. Axis: function count (symbol table, declaration
                    emission, effect summaries, clang function count).
      stmts  S  ->  ONE function with S statements in a single SSA chain.
                    Axis: per-function size (SSA numbering, one basic block,
                    LLVM mem2reg pressure). This is the axis most likely to
                    go superlinear.
      flow   N  ->  N methods on one record type + N while loops. Axis:
                    method/loop lowering and receiver handling.

    Everything is seeded from a fixed 31-bit LCG (see Get-Rand). No
    wall-clock, no GUIDs, no hashtable iteration order in the output: two
    runs on two machines produce byte-identical files. Check with
    `-Verify` which re-generates into a temp dir and compares hashes.

    Every program is written so that:
      * `nx check` is clean,
      * `nx build` succeeds,
      * the program terminates fast and prints a deterministic checksum,
      * all arithmetic is masked with `& 65535` so integer values stay in
        [0, 65535] and can never overflow i64 at run time. That keeps the
        generated code semantically boring (which is the point: we are
        measuring the compiler, not the program) while still exercising
        the unboxed Int path.

.PARAMETER Family
    funcs | stmts | flow | all

.PARAMETER Sizes
    Axis values per family. Default is the committed size ladder in
    README.md. Larger values are allowed for probing; they are not
    committed to the repo.

.PARAMETER OutDir
    Destination. Default <script dir>\programs.

.PARAMETER Verify
    Regenerate into a scratch dir and compare SHA256 of every file against
    OutDir. Proves determinism. Exits non-zero on any mismatch.
#>
[CmdletBinding()]
param(
    [ValidateSet('funcs', 'stmts', 'flow', 'all')]
    [string[]]$Family = @('all'),
    [string]$Sizes = 'funcs=50,200,800,2000;stmts=20,100,400,1500;flow=30,120,480',
    [string]$OutDir = (Join-Path $PSScriptRoot 'programs'),
    [int]$Seed = 20260902,
    [int]$StmtPerFn = 8,
    [int]$MethodStmts = 6,
    [int]$LoopBodyStmts = 5,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Deterministic PRNG: 31-bit Lehmer / MINSTD-style LCG (glibc constants).
# Int64 arithmetic throughout: 2147483647 * 1103515245 < 9.22e18, so it
# cannot overflow. Returns the top 24 bits, which are the well-mixed ones.
# ---------------------------------------------------------------------------
function Get-Rand([ref]$state, [int]$mod) {
    $state.Value = ([int64]$state.Value * 1103515245 + 12345) % 2147483648
    if ($mod -le 1) { return 0 }
    return [int](($state.Value -shr 8) % $mod)
}

# Operator/operand pair table. Every entry keeps its result inside
# [0, 65535] because the caller masks with `& 65535`.
$Ops = @(
    @{ Sym = '+'; K = 7 },     @{ Sym = '-'; K = 11 },   @{ Sym = '*'; K = 3 },
    @{ Sym = '%'; K = 1009 },  @{ Sym = '&'; K = 255 },  @{ Sym = '|'; K = 129 },
    @{ Sym = '^'; K = 61 }
)

# One accumulator step. $target receives the result, $lhs is the value read,
# $rhs (optional) is a third operand so a loop body can fold in its counter.
# The trailing `& 65535` is what keeps every value in [0,65535].
# $indent is a literal prefix so the same step works at function body depth
# (4 spaces) and inside a method body (8 spaces).
function Get-Step([ref]$state, [string]$target, [string]$lhs, [string]$rhs, [string]$indent = '    ') {
    $oi = Get-Rand $state $Ops.Count
    $sym = $Ops[$oi].Sym
    $a = 1 + (Get-Rand $state 97)
    $b = 1 + (Get-Rand $state 89)
    if ($rhs) {
        return ($indent + $target + ' = ((' + $lhs + ' ' + $sym + ' ' + $a + ') ' + $sym + ' ' + $b + ' + ' + $rhs + ') & 65535')
    }
    return ($indent + $target + ' = ((' + $lhs + ' ' + $sym + ' ' + $a + ') ' + $sym + ' ' + $b + ') & 65535')
}

function New-Sb {
    return [System.Text.StringBuilder]::new(1MB)
}

function Add-Line($sb, [string]$line) { [void]$sb.AppendLine($line) }

function Add-Header($sb, [string]$what, [int]$axis, [int]$seed) {
    Add-Line $sb "# ----------------------------------------------------------------"
    Add-Line $sb "# GENERATED FILE - do not edit. Produced by bench/compiler/gen.ps1"
    Add-Line $sb "# family=$what axis=$axis seed=$seed"
    Add-Line $sb "# Deterministic: byte-identical on every run and every machine."
    Add-Line $sb "# Every arithmetic result is masked with & 65535 so values stay"
    Add-Line $sb "# in [0,65535]; the program cannot overflow i64 or hang."
    Add-Line $sb "# ----------------------------------------------------------------"
    Add-Line $sb ""
}

# ---------------------------------------------------------------------------
# funcs: axis = number of independent functions.
# ---------------------------------------------------------------------------
function New-Funcs([int]$n, [int]$stmts, [int]$seed) {
    $st = [ref]([int64]($seed -band 0x7fffffff))
    $sb = New-Sb
    Add-Header $sb 'funcs' $n $seed
    Add-Line $sb "# axis: $n functions, $stmts statements each."
    Add-Line $sb ""
    for ($i = 0; $i -lt $n; $i++) {
        Add-Line $sb "fn f$i(a, b):"
        Add-Line $sb "    t = (a + b + $i) & 65535"
        for ($k = 1; $k -lt $stmts; $k++) {
            Add-Line $sb (Get-Step $st 't' 't' '')
        }
        Add-Line $sb "    return t"
        Add-Line $sb ""
    }
    # Driver: every function is called exactly once, so nothing is
    # dead code. Depth-2 call graph, breadth N.
    Add-Line $sb "total = 0"
    for ($i = 0; $i -lt $n; $i++) {
        $x = Get-Rand $st 512
        $y = Get-Rand $st 512
        Add-Line $sb "total = (total + f$i($x, $y)) & 65535"
    }
    Add-Line $sb "print(total)"
    Add-Line $sb ""
    return $sb.ToString()
}

# ---------------------------------------------------------------------------
# stmts: axis = statements in a single function (one long SSA chain).
# ---------------------------------------------------------------------------
function New-Stmts([int]$s, [int]$seed) {
    $st = [ref]([int64]($seed -band 0x7fffffff))
    $sb = New-Sb
    Add-Header $sb 'stmts' $s $seed
    Add-Line $sb "# axis: 1 function, $s statements in one straight-line SSA chain."
    Add-Line $sb ""
    Add-Line $sb "fn work(a, b):"
    Add-Line $sb "    t = (a + b) & 65535"
    for ($k = 1; $k -lt $s; $k++) {
        Add-Line $sb (Get-Step $st 't' 't' '')
    }
    Add-Line $sb "    return t"
    Add-Line $sb ""
    Add-Line $sb "# seed[] is filled at run time with push, so seed[i] is a heap read"
    Add-Line $sb "# through the runtime rather than a constant. Without this the whole"
    Add-Line $sb "# chain above folds to one literal and the generated binary is the"
    Add-Line $sb "# same size for every s, which would hide clang's real work here."
    Add-Line $sb "seed = []"
    Add-Line $sb "i = 0"
    Add-Line $sb "while i < 4:"
    Add-Line $sb "    push(seed, (i * 7919 + 13) & 65535)"
    Add-Line $sb "    i = i + 1"
    Add-Line $sb ""
    Add-Line $sb "# The compile-time axis is the chain above, not the loop count, which"
    Add-Line $sb "# is pinned at 4 so the program runs instantly."
    Add-Line $sb "fn step(k):"
    Add-Line $sb "    i = 0"
    Add-Line $sb "    acc = 0"
    Add-Line $sb "    while i < 4:"
    Add-Line $sb "        acc = (acc + work(seed[i], i + k)) & 65535"
    Add-Line $sb "        i = i + 1"
    Add-Line $sb "    return acc"
    Add-Line $sb ""
    Add-Line $sb "print(step(7))"
    Add-Line $sb "print(step(11))"
    Add-Line $sb ""
    return $sb.ToString()
}

# ---------------------------------------------------------------------------
# flow: axis = method count + loop count, receiver lowering.
# ---------------------------------------------------------------------------
function New-Flow([int]$n, [int]$mst, [int]$lst, [int]$seed) {
    $st = [ref]([int64]($seed -band 0x7fffffff))
    $sb = New-Sb
    Add-Header $sb 'flow' $n $seed
    Add-Line $sb "# axis: 1 record type, $n methods ($mst statements each), $n loops"
    Add-Line $sb "# ($lst statements per loop body)."
    Add-Line $sb ""
    Add-Line $sb "type Cell:"
    Add-Line $sb "    v: Int"
    Add-Line $sb "    w: Int"
    Add-Line $sb ""
    Add-Line $sb "impl Cell:"
    for ($i = 0; $i -lt $n; $i++) {
        Add-Line $sb "    fn m$i(self, k):"
        Add-Line $sb "        t = (self.v + self.w + k) & 65535"
        for ($q = 1; $q -lt $mst; $q++) {
            Add-Line $sb (Get-Step $st 't' 't' '' '        ')
        }
        Add-Line $sb "        return t"
        Add-Line $sb ""
    }
    Add-Line $sb "c = Cell(7, 11)"
    Add-Line $sb "total = 0"
    for ($i = 0; $i -lt $n; $i++) {
        Add-Line $sb "# loop $i"
        Add-Line $sb "i$i = 0"
        Add-Line $sb "acc$i = 0"
        Add-Line $sb "while i$i < 3:"
        Add-Line $sb "    acc$i = (acc$i + c.m$i(i$i + $i)) & 65535"
        for ($q = 1; $q -lt $lst; $q++) {
            Add-Line $sb (Get-Step $st "acc$i" "acc$i" "i$i" '    ')
        }
        Add-Line $sb "    i$i = i$i + 1"
        Add-Line $sb "total = (total + acc$i) & 65535"
        Add-Line $sb ""
    }
    Add-Line $sb "print(total)"
    Add-Line $sb "print(c.m0(1), c.m0(2))"
    Add-Line $sb ""
    return $sb.ToString()
}

# ---------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------
function Get-Families($f) {
    if ($f -contains 'all') { return @('funcs', 'stmts', 'flow') }
    return $f
}

function Get-Sizes([string]$spec, [string]$fam) {
    foreach ($part in $spec -split ';') {
        $kv = $part -split '='
        if ($kv.Count -eq 2 -and $kv[0].Trim() -eq $fam) {
            return @($kv[1] -split ',' | ForEach-Object { [int]$_.Trim() })
        }
    }
    return @()
}

$script:Manifest = @()

function Save-Program([string]$fam, [int]$axis, [string]$text) {
    $name = "$fam-$axis.nx"
    $path = Join-Path $OutDir $name
    # UTF8 without BOM: PowerShell 5.1 defaults to a BOM otherwise, and the
    # lexer would then see a stray codepoint at offset 0.
    [System.IO.File]::WriteAllText($path, $text, (New-Object System.Text.UTF8Encoding $false))
    $info = Get-Item $path
    $sha = (Get-FileHash $path -Algorithm SHA256).Hash
    $lines = ([regex]::Matches($text, "`n")).Count + 1
    $script:Manifest += [ordered]@{
        file            = $name
        family          = $fam
        axis            = $axis
        bytes           = $info.Length
        lines           = $lines
        sha256          = $sha
        sha256_short    = $sha.Substring(0, 12)
    }
    Write-Host ("  {0,-16} {1,9:N0} bytes {2,7:N0} lines  {3}" -f $name, $info.Length, $lines, $sha.Substring(0, 12))
    return $name
}

if ($Verify) {
    # Determinism gate: regenerate the committed set into a scratch dir and
    # compare hashes. Run this after any edit to gen.ps1.
    $tmp = Join-Path $env:TEMP "nxgen-verify"
    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $tmp | Out-Null
    & $PSCommandPath -Family $Family -Sizes $Sizes -OutDir $tmp -Seed $Seed `
        -StmtPerFn $StmtPerFn -MethodStmts $MethodStmts -LoopBodyStmts $LoopBodyStmts
    $bad = 0
    foreach ($m in $script:Manifest) {
        $orig = Join-Path $OutDir $m.file
        if (-not (Test-Path $orig)) { Write-Host "  MISSING in $OutDir : $($m.file)" -ForegroundColor Red; $bad++; continue }
        $h = (Get-FileHash $orig -Algorithm SHA256).Hash
        if ($h -ne $m.sha256) {
            Write-Host "  DIFFERS: $($m.file)  committed=$($h.Substring(0,12))  regen=$($m.sha256.Substring(0,12))" -ForegroundColor Red
            $bad++
        } else {
            Write-Host "  ok: $($m.file)" -ForegroundColor DarkGray
        }
    }
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    if ($bad -gt 0) { Write-Host "VERIFY FAILED: $bad file(s) differ" -ForegroundColor Red; exit 1 }
    Write-Host "VERIFY OK: all $($script:Manifest.Count) file(s) byte-identical on regeneration" -ForegroundColor Green
    exit 0
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$script:Manifest = @()

foreach ($fam in (Get-Families $Family)) {
    # NOTE: this local must NOT be called $sizes -- PowerShell variable names
    # are case-insensitive, so that would clobber the $Sizes script parameter on
    # the first iteration and silently shrink the spec to its first element.
    $axisList = @(Get-Sizes $Sizes $fam)
    if ($axisList.Count -eq 0) {
        Write-Warning "no sizes given for family '$fam'; skipping"
        continue
    }
    Write-Host "family $fam  axis=$($axisList -join ',')  seed=$Seed" -ForegroundColor Cyan
    foreach ($n in $axisList) {
        switch ($fam) {
            'funcs' { $text = New-Funcs $n $StmtPerFn $Seed }
            'stmts' { $text = New-Stmts $n $Seed }
            'flow'  { $text = New-Flow $n $MethodStmts $LoopBodyStmts $Seed }
        }
        Save-Program $fam $n $text | Out-Null
    }
}

$manifestPath = Join-Path $OutDir 'manifest.json'
$script:Manifest | ConvertTo-Json -Depth 4 | Set-Content -Path $manifestPath -Encoding UTF8
Write-Host ""
Write-Host ("manifest -> {0}" -f $manifestPath) -ForegroundColor DarkGray
$total = 0
foreach ($m in $script:Manifest) { $total += $m['bytes'] }
Write-Host ("generated {0} file(s), {1:N0} bytes total" -f $script:Manifest.Count, $total) -ForegroundColor DarkGray