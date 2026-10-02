# Verifies every example three ways: the interpreter, a native build, and
# a native build with unboxing disabled (NX_NOUNBOX=1). All three must
# produce identical output, or the compiler is lying to somebody.
#
# Usage: pwsh -File tools\verify.ps1
$ErrorActionPreference = 'Continue'
$nx = "$env:USERPROFILE\.cargo\bin\nx.exe"
$root = Split-Path -Parent $PSScriptRoot
$examples = Get-ChildItem "$root\examples\*.nx" | Sort-Object Name
$fail = 0

# Native command stderr arrives as PowerShell error records; anything on
# this pattern is build chatter, not program output.
function Get-Clean {
    param([scriptblock]$Cmd)
    $ErrorActionPreference = 'Continue'
    $out = & $Cmd 2>&1 |
        Where-Object {
            "$_" -notmatch 'CategoryInfo|FullyQualifiedErrorId|At line|^\s*\+|^\s*$' -and
            "$_" -notmatch 'warning: overriding the module target triple|warning generated|^nx: built '
        }
    return @($out | ForEach-Object { "$_" })
}

foreach ($ex in $examples) {
    $name = $ex.Name
    $stem = $ex.BaseName
    $work = Join-Path $env:TEMP "nxverify_$stem"
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    # Copy the example plus the shared module directory, so cross-module
    # examples resolve.
    Copy-Item $ex.FullName $work
    if (Test-Path "$root\examples\modules") {
        Copy-Item -Recurse "$root\examples\modules" $work
    }

    $interp = Get-Clean { & $nx "$work\$name" }

    Push-Location $work
    Get-Clean { & $nx build $name } | Out-Null
    $buildOk = (Test-Path "$stem.exe")
    Pop-Location
    if (-not $buildOk) {
        Write-Host "FAIL $name : native build failed"
        $fail++
        continue
    }
    $native = Get-Clean { & "$work\$stem.exe" }

    # Rebuild with unboxing off; the stamp makes nx skip an unchanged build,
    # so the old executable has to go first.
    $env:NX_NOUNBOX = '1'
    Push-Location $work
    Remove-Item "$stem.exe", "$stem.exe.nxstamp" -ErrorAction SilentlyContinue
    Get-Clean { & $nx build $name } | Out-Null
    $rebuildOk = (Test-Path "$stem.exe")
    Pop-Location
    $env:NX_NOUNBOX = ''
    if (-not $rebuildOk) {
        Write-Host "FAIL $name : NX_NOUNBOX build failed"
        $fail++
        continue
    }
    $unboxed = Get-Clean { & "$work\$stem.exe" }

    $d1 = Compare-Object -ReferenceObject $interp -DifferenceObject $native
    $d2 = Compare-Object -ReferenceObject $interp -DifferenceObject $unboxed
    if ($d1 -or $d2) {
        Write-Host "FAIL $name : paths disagree"
        foreach ($pair in @(@('native', $d1), @('NX_NOUNBOX', $d2))) {
            if ($pair[1]) {
                Write-Host "  interpreter vs $($pair[0]):"
                $pair[1] | ForEach-Object { Write-Host "    $($_.SideIndicator) $($_.InputObject)" }
            }
        }
        $fail++
    } else {
        Write-Host ("ok   {0,-16} {1} lines" -f $name, $interp.Count)
    }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}

Write-Host ""
if ($fail -eq 0) {
    Write-Host "all $($examples.Count) examples agree across interpreter / native / NX_NOUNBOX"
} else {
    Write-Host "$fail of $($examples.Count) examples disagree"
    exit 1
}