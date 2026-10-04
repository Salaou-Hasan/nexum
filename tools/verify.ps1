# Differential harness: every example is built twice as a native executable and
# both runs must produce identical output.
#
#   1. the default build      (unboxing on)
#   2. NX_NOUNBOX=1           (every value boxed)
#
# The second build exists to prove the unboxing pass is a representation
# change and not a language change. If they ever disagree, `NX_NOUNBOX` has
# become a second language.
#
# There is deliberately NO interpreter axis. The tree-walking interpreter was
# removed; Nexum compiles ahead-of-time and has exactly one execution model.
# The compiler that produces the reference output is the same compiler whose
# IR is being checked, so this harness cannot prove the compiler correct on
# its own. It proves one thing: that the unboxing decision changes nothing
# observable. Correctness is pinned by the expected-value tests in
# compiler/nx-e2e, not here.
#
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File tools\verify.ps1
#
# ASCII only: PowerShell 5.1 reads a BOM-less .ps1 as ANSI.
$ErrorActionPreference = 'Continue'

$root = Split-Path -Parent $PSScriptRoot

# Build the compiler under test. Using an installed copy from $CARGO_HOME\bin
# would let this report all-green while testing a binary from many commits
# ago, which is worse than not testing at all.
# Honour an inherited CARGO_TARGET_DIR so this harness can run against an
# out-of-tree build instead of fighting another process for target\debug.
if ([string]::IsNullOrWhiteSpace($env:CARGO_TARGET_DIR)) {
    $env:CARGO_TARGET_DIR = Join-Path $root 'target'
}
$nx = Join-Path (Join-Path $env:CARGO_TARGET_DIR 'debug') 'nx.exe'
Push-Location $root
& cargo build -q -p nx-driver --offline 2>&1 | Out-Null
$built = $LASTEXITCODE
Pop-Location
if ($built -ne 0 -or -not (Test-Path $nx)) {
    Write-Host "FATAL: could not build the compiler under test at $nx (cargo exit $built)"
    exit 2
}

# The corpus is an explicit list, not a glob. A glob silently skips whatever
# a naming convention happens to exclude; examples/modules/main.nx is exactly
# that kind of file, and a recursive glob would also pick up module parts that
# are not entry points.
$examples = @(
    'boundaries.nx', 'comments.nx', 'control.nx', 'fib.nx', 'flex.nx',
    'funcs.nx',
    'hello.nx', 'lists.nx', 'methods.nx', 'records.nx',
    'search.nx', 'syntax.nx', 'unbox.nx',
    'modules/main.nx'
)

$fail = 0

# Run an executable and capture its raw stdout as a single string, plus its
# exit code. Output goes to a file rather than through PowerShell's pipeline,
# because a native command's stderr becomes an error record here and any
# pipeline filter would then have to guess what was program output and what
# was build chatter. Bytes in, bytes out.
function Invoke-Program {
    param([string]$Exe, [string]$OutFile)
    & $Exe 1> $OutFile 2> $null
    return @{ Code = $LASTEXITCODE; Out = [IO.File]::ReadAllText($OutFile) }
}

function Normalize([string]$s) {
    # CRLF and a trailing newline are not semantic. Everything else is.
    return ($s -replace "`r`n", "`n").TrimEnd("`n")
}

foreach ($rel in $examples) {
    $name = Split-Path $rel -Leaf
    $stem = [IO.Path]::GetFileNameWithoutExtension($rel)
    $src = Join-Path (Join-Path $root 'examples') $rel
    if (-not (Test-Path $src)) {
        Write-Host "FAIL $rel : example missing from the corpus list"
        $fail++
        continue
    }

    $work = Join-Path $env:TEMP "nxverify_$stem"
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $work | Out-Null

    # An entry inside a subdirectory (examples/modules/main.nx) must keep its
    # siblings, because its imports resolve relative to its own directory.
    # Flattening it to the work root breaks every import it has.
    $relDir = Split-Path $rel -Parent
    $entryDir = $work
    if ($relDir) {
        $entryDir = Join-Path $work $relDir
        New-Item -ItemType Directory -Force -Path $entryDir | Out-Null
        $srcDir = Join-Path (Join-Path $root 'examples') $relDir
        Copy-Item -Recurse -Force (Join-Path $srcDir '*') $entryDir
    }
    else {
        Copy-Item $src (Join-Path $work $name) -Force
    }

    Push-Location $entryDir
    try {
        $exe = Join-Path $work "$stem.exe"

        # Build 1: default.
        & $nx build $name -o $exe 1> build1.log 2>&1
        $rc1 = $LASTEXITCODE
        if ($rc1 -ne 0 -or -not (Test-Path $exe)) {
            Write-Host "FAIL $rel : native build failed"
            Get-Content build1.log -ErrorAction SilentlyContinue |
                Select-Object -Last 5 | ForEach-Object { Write-Host "      $_" }
            $fail++
            continue
        }
        $a = Invoke-Program $exe (Join-Path $work 'out1.txt')

        # Build 2: unboxing off. The stamp file makes nx skip an unchanged
        # build, and NX_NOUNBOX is deliberately absent from the stamp, so the
        # previous executable and its stamp both have to go.
        $env:NX_NOUNBOX = '1'
        Remove-Item $exe, "$exe.nxstamp" -Force -ErrorAction SilentlyContinue
        & $nx build $name -o $exe 1> build2.log 2>&1
        $rc2 = $LASTEXITCODE
        $env:NX_NOUNBOX = ''
        if ($rc2 -ne 0 -or -not (Test-Path $exe)) {
            Write-Host "FAIL $rel : NX_NOUNBOX build failed"
            $fail++
            continue
        }
        $b = Invoke-Program $exe (Join-Path $work 'out2.txt')

        # Both runs must succeed. A program that fails identically on both
        # paths would otherwise compare equal and report ok.
        if ($a.Code -ne 0) {
            Write-Host "FAIL $rel : native run exited $($a.Code)"
            Write-Host "      $($a.Out.Trim())"
            $fail++
            continue
        }
        if ($b.Code -ne 0) {
            Write-Host "FAIL $rel : NX_NOUNBOX run exited $($b.Code)"
            Write-Host "      $($b.Out.Trim())"
            $fail++
            continue
        }

        $oa = Normalize $a.Out
        $ob = Normalize $b.Out
        if ($oa -ne $ob) {
            Write-Host "FAIL $rel : unboxed and NX_NOUNBOX disagree"
            Write-Host "      default   : $($oa -replace "`n", ' | ')"
            Write-Host "      NX_NOUNBOX: $($ob -replace "`n", ' | ')"
            $fail++
            continue
        }
        if ([string]::IsNullOrWhiteSpace($oa)) {
            Write-Host "FAIL $rel : produced no output, so this proves nothing"
            $fail++
            continue
        }
        $count = ($oa -split "`n").Count
        Write-Host ("ok   {0,-18} {1} lines" -f $rel, $count)
    }
    finally {
        Pop-Location
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }
}

Write-Host ""
if ($fail -eq 0) {
    Write-Host "all $($examples.Count) examples agree across default / NX_NOUNBOX"
    exit 0
}
Write-Host "$fail of $($examples.Count) examples failed"
exit 1