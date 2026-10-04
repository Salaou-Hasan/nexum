# Proves the Argone gate actually rejects false completion.
#
# "Never partially implemented" is only worth something if it is enforced.
# This script tries to fool the gate the four ways the prompt names as
# invalid grounds for declaring success -- scaffolding, a partial status,
# unchecked boxes, and a build that merely compiles -- and fails if any of
# them opens the gate.
#
# It mutates docs/ARGONE-STATUS.md and always restores it.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$gate = Join-Path $PSScriptRoot 'argone-gate.ps1'
# Join-Path, never a literal separator -- see argone-gate.ps1 for why.
$status = Join-Path (Join-Path $root 'docs') 'ARGONE-STATUS.md'
# [IO.Path]::GetTempPath, not $env:TEMP: TEMP is a Windows-only
# variable, so Join-Path on it throws on Linux and the whole step dies.
# GetTempPath returns %TEMP% on Windows and /tmp/ everywhere else.
$backup = Join-Path ([IO.Path]::GetTempPath()) 'argone-status-backup.md'

Copy-Item $status $backup -Force

function Invoke-Gate {
    $exe = if ($PSVersionTable.PSEdition -eq 'Core') { 'pwsh' } else { 'powershell' }
    $out = & $exe -NoProfile -ExecutionPolicy Bypass -File $gate -Enforce 2>&1
    return @{ Code = $LASTEXITCODE; Out = ($out | ForEach-Object { "$_" }) }
}

function Set-Status([string]$text) {
    [IO.File]::WriteAllText($status, $text)
}

$original = [IO.File]::ReadAllText($backup)
$fails = 0
$passes = 0

function Expect([string]$name, [bool]$shouldBeBlocked, [string]$expectText) {
    $r = Invoke-Gate
    $blocked = ($r.Code -ne 0)
    $namedIt = ($r.Out -join "`n") -match [regex]::Escape($expectText)
    $ok = if ($shouldBeBlocked) { $blocked -and $namedIt } else { -not $blocked }
    if ($ok) {
        Write-Host ("  PASS  {0}" -f $name)
        $script:passes++
    } else {
        Write-Host ("  FAIL  {0}" -f $name)
        Write-Host ("        exit={0} blocked={1} namedIt={2}" -f $r.Code, $blocked, $namedIt)
        ($r.Out | Where-Object { $_ -match 'BLOCKER|  - ' } | Select-Object -First 3) | ForEach-Object { Write-Host "        $_" }
        $script:fails++
    }
}

try {
    Write-Host ""
    Write-Host "Argone gate: adversarial tests"
    Write-Host "=" * 72

    # 1. The honest state must block, and say so.
    Set-Status $original
    Expect "honest state is blocked" $true "ARGONE NOT COMPLETE"

    # 2. Scaffolding: claim a task complete with nothing checked off.
    # The target is the first task that is still open, so this keeps
    # working as tasks genuinely complete (a fixed task letter would rot
    # the day that task finishes).
    $open = [regex]::Match($original, '(?m)^## (?:Task 0|[A-N]) - .*\r?\n\r?\nStatus: not started')
    if (-not $open.Success) {
        Write-Host "  FAIL  scaffolding cannot claim complete"
        Write-Host "        no open task left to scaffold; the suite needs a live target"
        $script:fails++
    } else {
        Set-Status ($original.Replace($open.Value, $open.Value.Replace('not started', 'complete')))
        Expect "scaffolding cannot claim complete" $true "claims complete but"
    }

    # 3. A partial status word.
    Set-Status ($original -replace '(?m)^Status: not started', 'Status: in progress')
    Expect "'in progress' is rejected" $true "partial state"

    Set-Status ($original -replace '(?m)^Status: not started', 'Status: mostly done')
    Expect "'mostly done' is rejected" $true "partial state"

    Set-Status ($original -replace '(?m)^Status: not started', 'Status: blocked')
    Expect "'blocked' is not a status" $true "partial state"

    # 4. All boxes ticked and claimed complete, but the Evidence lines are
    #    gone. This is "it compiles and the boxes are green, ship it". Note
    #    that ticking boxes alone is not enough to reach this rule -- with
    #    the status still `not started` the gate blocks on task count, not
    #    on evidence -- so the status has to be flipped too.
    $noEvidence = $original -replace '(?m)^Status: not started', 'Status: complete'
    # `[ ]` rather than `\s`: in multiline mode `\s` also matches the
    # newline, so `\s*` can walk past a blank line and tick the wrong box.
    $noEvidence = $noEvidence -replace '(?m)^([ ]*)- \[ \]', '$1- [x]'
    $noEvidence = $noEvidence -replace '(?m)^[ ]*Evidence:.*\r?\n', ''
    Set-Status $noEvidence
    Expect "all-ticked without evidence is blocked" $true "Evidence"

    # 5. One task genuinely complete: still blocked, because the rest
    #    are not. This is the check that stops "one task done" reading
    #    as "done". The expected count derives from the honest state
    #    (tasks already complete stay complete), so it keeps working as
    #    the stage advances. B is hardcoded below: retarget this test
    #    the day B genuinely completes.
    $baseDone = ([regex]::Matches($original, '(?m)^Status: complete')).Count
    $oneDone = $original
    $oneDone = $oneDone -replace '(?m)^(## B - Baselines\r?\n\r?\nStatus: )not started', '${1}complete'
    $oneDone = $oneDone -replace '(?m)^- \[ \] Every benchmark category', '- [x] Every benchmark category'
    $oneDone = $oneDone -replace '(?m)^- \[ \] bench/baseline.json committed', '- [x] bench/baseline.json committed'
    $oneDone = $oneDone -replace '(?m)^- \[ \] clang-vs-Nexum codegen split', '- [x] clang-vs-Nexum codegen split'
    $oneDone = $oneDone -replace '(?m)^- \[ \] Noise characterised', '- [x] Noise characterised'
    $oneDone = $oneDone -replace '(?m)^[ ]*Evidence:[ ]*\r?$', '      Evidence: commit def5678'
    Set-Status $oneDone
    Expect "one task done does not open the gate" $true "tasks complete: $($baseDone + 1) / 15"

    # 6. A missing task must be noticed, not silently ignored.
    $missing = $original -replace '(?ms)^## N - .*$', ''
    Set-Status $missing
    Expect "a missing task is a blocker" $true "missing from the status file"

    # 7. A task with no Status line at all.
    $noStatus = $original -replace '(?m)^Status: not started\r?\n', ''
    Set-Status $noStatus
    Expect "a missing status is a blocker" $true "no Status line"

    # 8. Everything done: the gate opens. If this fails, the gate is
    #    unreachable and the whole mechanism is theatre.
    $allDone = $original -replace '(?m)^Status: not started', 'Status: complete'
    $allDone = $allDone -replace '(?m)^([ ]*)- \[ \]', '$1- [x]'
    $allDone = $allDone -replace '(?m)^[ ]*Evidence:[ ]*\r?$', '      Evidence: commit abc1234'
    Set-Status $allDone
    Expect "a fully complete stage opens the gate" $false "ARGONE COMPLETE"
}
finally {
    Set-Status $original
}

Write-Host "=" * 72
Write-Host ("{0} passed, {1} failed" -f $passes, $fails)
Write-Host ""

if ($fails -gt 0) { exit 1 }
exit 0
