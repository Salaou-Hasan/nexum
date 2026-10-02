# ARGONE stage gate.
#
# Reads docs/ARGONE-STATUS.md and decides whether the Argone stage is
# complete. Argone is a hard gate: while this script fails, Stage 4 through
# Stage 8 are locked.
#
# The point of this script is to make "never partially implemented"
# mechanical rather than a promise. It exists to reject the four things the
# prompt names as invalid grounds for declaring success:
#
#   - scaffolding exists
#   - placeholder IR
#   - a demo-only path that reaches MLIR
#   - the compiler builds
#
# Usage:
#   pwsh -File tools\argone-gate.ps1              # report only
#   pwsh -File tools\argone-gate.ps1 -Enforce     # exit 1 when incomplete
#
# `-Enforce` is what CI runs. Reporting without it always exits 0, so a
# developer can see where things stand without a red build for work that is
# legitimately in progress.

param([switch]$Enforce)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
# Build paths with Join-Path, never with a literal separator. PowerShell on
# Unix treats `\` as an ordinary filename character, so a hand-written
# `docs\ARGONE-STATUS.md` resolves to a file that does not exist there and
# the gate exits 2. That silently made this script fail on ubuntu CI.
$statusPath = Join-Path (Join-Path $root 'docs') 'ARGONE-STATUS.md'

if (-not (Test-Path $statusPath)) {
    Write-Host "FATAL: missing $statusPath"
    exit 2
}

$lines = [IO.File]::ReadAllLines($statusPath)

# The tasks the plan defines. Task 0 is semantic correctness: the audit
# found eleven reachable programs whose semantics were wrong, and
# backend disagree, and the fix gates every representation change.
$expected = @(
    'Task 0', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H',
    'I', 'J', 'K', 'L', 'M', 'N'
)

# States that mean "partially implemented". All of them are rejected on
# purpose: a stage that admits a third state is a stage that gets left
# halfway. The prompt forbids exactly this.
$partialStates = @('in progress', 'partial', 'mostly', 'wip', '~50', 'pending', 'blocked')

$tasks = @()
$current = $null
foreach ($line in $lines) {
    # Match on the task id only. Do not put a non-ASCII character (an
    # em-dash) in this pattern: PowerShell reads a BOM-less .ps1 as ANSI,
    # so it would arrive here as two unrelated bytes and silently stop
    # matching. This script is deliberately pure ASCII.
    if ($line -match '^##\s+(Task 0|[A-P])\b') {
        $current = [ordered]@{
            Id       = $Matches[1]
            Title    = ($line -replace '^##\s+', '')
            Status   = $null
            Unmet    = 0
            Total    = 0
            Evidence = $false
        }
        $tasks += [pscustomobject]$current
        $current = $tasks[-1]
        continue
    }
    if ($null -eq $current) { continue }

    if ($line -match '^Status:\s*\*{0,2}(.+?)\*{0,2}\s*$') {
        $current.Status = $Matches[1].Trim()
        continue
    }
    # Gate items live under a task until the next heading. Only count the
    # `- [ ]` / `- [x]` checkbox lines, and require an Evidence line for
    # every checked item group.
    if ($line -match '^\s*-\s\[( |x|X)\]\s') {
        $current.Total++
        if ($Matches[1] -eq ' ') { $current.Unmet++ }
    }
    if ($line -match '^\s*Evidence:\s') {
        if ($line -replace '^\s*Evidence:\s*', '' -match '\S') { $current.Evidence = $true }
    }
}

$problems = @()

# 1. Every expected task must be present.
foreach ($e in $expected) {
    if (-not ($tasks | Where-Object { $_.Id -eq $e })) {
        $problems += "task '$e' is missing from the status file"
    }
}

Write-Host ""
Write-Host "ARGONE stage gate"
Write-Host ("=" * 72)
Write-Host ("{0,-8} {1,-14} {2,-10} {3}" -f 'TASK', 'STATUS', 'GATE', 'TITLE')
Write-Host ("-" * 72)

$done = 0
foreach ($t in $tasks) {
    $isDone = $t.Status -and ($t.Status.ToLower() -eq 'complete')
    $gate = if ($t.Total -eq 0) { 'n/a' } else { "$($t.Total - $t.Unmet)/$($t.Total)" }

    if ($isDone) { $done++ }
    Write-Host ("{0,-8} {1,-14} {2,-10} {3}" -f $t.Id, $t.Status, $gate, $t.Title)
}

# 2. A task may only claim `complete` if its gate is fully met and it has
#    evidence. This is what makes "scaffolding exists" insufficient: a
#    scaffold has no checked items and no evidence.
foreach ($t in $tasks) {
    if (-not $t.Status) {
        $problems += "task '$($t.Id)' has no Status line"
        continue
    }
    $s = $t.Status.ToLower()
    foreach ($p in $partialStates) {
        if ($s -like "*$p*") {
            $problems += "task '$($t.Id)' status '$($t.Status)' is a partial state; only 'complete' or 'not started' are allowed"
        }
    }
    if ($s -eq 'complete') {
        if ($t.Unmet -gt 0) {
            $problems += "task '$($t.Id)' claims complete but $($t.Unmet) gate item(s) are unchecked"
        }
        if (-not $t.Evidence) {
            $problems += "task '$($t.Id)' claims complete but carries no Evidence line"
        }
    }
}

# 3. The stage is complete only when all seventeen tasks are.
$complete = ($done -eq $expected.Count)

Write-Host ("-" * 72)
Write-Host ("tasks complete: $done / $($expected.Count)")
Write-Host ""

if ($problems.Count -gt 0) {
    Write-Host "GATE BLOCKERS:"
    foreach ($p in $problems) { Write-Host "  - $p" }
    Write-Host ""
}

if ($complete -and $problems.Count -eq 0) {
    Write-Host "ARGONE COMPLETE"
    Write-Host "Stage 4 -> 8 roadmap may resume, per docs/ARGONE.md."
    Write-Host ""
    if ($Enforce) { exit 0 } else { exit 0 }
}

Write-Host "ARGONE NOT COMPLETE"
Write-Host "Stage 4 -> 8 roadmap is LOCKED."
Write-Host ""
Write-Host "This is the expected state while work is in progress. Argone is a"
Write-Host "hard gate: do not resume Stage 4-8, and do not mark a task complete"
Write-Host "from scaffolding, placeholder IR, a demo that reaches MLIR, or a"
Write-Host "compiler that merely builds."
Write-Host ""

if ($Enforce) {
    Write-Host "(run without -Enforce to report without failing the build)"
    exit 1
}
exit 0
