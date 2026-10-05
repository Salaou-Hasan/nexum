# Simulates VS Code's Enter-indent pipeline for Nexum, mirroring the
# real sources (cursorTypeEditOperations.ts EnterOperation._enter,
# autoIndent.ts getIndentForEnter/getInheritIndentForLine,
# enterAction.ts getEnterAction):
#  1. onEnterRules in order (beforeText only; missing clauses don't care)
#  2. matched None  -> new line keeps the CURRENT line's own indent
#  3. matched Indent -> current indent shifted one level
#  4. no match -> backward walk past blank lines to the nearest content
#     line P: increase match -> P.indent + 1 level; otherwise P.indent
#     (honorIntentionalIndent). Case-sensitive, like JS RegExp.
param(
    [string]$ConfigPath = "editors/vscode-nexum/language-configuration.json"
)

$ErrorActionPreference = "Stop"
$cfg = Get-Content $ConfigPath -Raw | ConvertFrom-Json
$inc = $cfg.indentationRules.increaseIndentPattern
$dec = $cfg.indentationRules.decreaseIndentPattern
$rules = @($cfg.onEnterRules | ForEach-Object {
    [pscustomobject]@{ Before = $_.beforeText; Action = $_.action.indentAction }
})

function Get-Indent([string]$line, [int]$col) {
    # getIndentationAtPosition: leading whitespace truncated to cursor
    $m = [regex]::Match($line, '^\s*')
    $ws = $m.Value
    if ($ws.Length -gt ($col - 1)) { return $ws.Substring(0, $col - 1) }
    return $ws
}

function Enter-Indent([string[]]$doc, [int]$line, [int]$col) {
    $text = $doc[$line - 1]
    $before = $text.Substring(0, [Math]::Min($col - 1, $text.Length))
    foreach ($r in $rules) {
        if ($before -cmatch $r.Before) {
            $own = Get-Indent $text $col
            if ($r.Action -eq "indent") { return $own + "    " }
            return $own  # none
        }
    }
    # fallback: nearest preceding non-blank line
    $p = $line - 1
    while ($p -ge 1 -and $doc[$p - 1] -match '^\s*$') { $p-- }
    if ($p -lt 1) { return "" }
    $pline = $doc[$p - 1]
    $pindent = ([regex]::Match($pline, '^\s*')).Value
    if ($pline -cmatch $inc) { return $pindent + "    " }
    return $pindent
}

$fib = @(
    "fn fib(n):",
    "    if n <= 1:",
    "        return n",
    "    else:",
    "        return fib(n - 1) + fib(n - 2)",
    "",
    "",
    "",
    "",
    "",
    "",
    "print(fib(30))"
)
$inblock = @(
    "fn fib(n):",
    "    if n <= 1:",
    "        return n",
    "    "
)

function Show([string]$name, [string[]]$doc, [int]$line, [int]$col, [int]$want) {
    $got = (Enter-Indent $doc $line $col).Length
    $ok = if ($got -eq $want) { "ok  " } else { "FAIL" }
    "{0} {1,-58} got col {2} want col {3}" -f $ok, $name, $got, $want
}

"rules in config: $($rules.Count) [$($rules.Action -join ', ')]"
Show "blank line 8 col 0, Enter (the report)" $fib 8 1 0
Show "in-block blank col 4, Enter (must keep 4)" $inblock 4 5 4
Show "end of 'fn fib(n):', Enter (must indent)" $fib 1 11 4
Show "end of return line, Enter (must keep 8)" $fib 5 44 8
Show "col 0 of print(), Enter (blank line above code)" $fib 12 1 0