# A faithful port of VS Code's Enter-key indentation pipeline, run over a
# corpus of cursor positions, plus a differential against the language
# configuration VS Code ships for Python.
#
# Why this file exists at all: the previous version of this script was a
# *model* of the pipeline rather than a port of it, and it read
# `onEnterRules[i].action.indentAction`. VS Code does not read that key. It
# reads `action.indent` (languageConfigurationExtensionPoint.ts:348-358) and
# for anything else logs
#
#   expected `onEnterRules[i].action.indent` to be 'none', 'indent',
#   'indentOutdent' or 'outdent'.
#
# and then `continue`s, discarding the rule. So both of this repo's
# onEnterRules had never run: Enter after `fn f():` only indented because
# `indentationRules` happened to produce the same answer, and Enter on a
# blank line at column 0 fell through to `getIndentForEnter`, which
# recomputes indentation from the nearest preceding non-blank line -- and so
# put the cursor back inside the block it had just left. The script that
# was supposed to prevent exactly that agreed with the config, because it
# made the same mistake the config did.
#
# Ported from microsoft/vscode @ main, function by function:
#
#   schema      workbench/contrib/codeEditor/common/
#                 languageConfigurationExtensionPoint.ts:336-395
#                 -- onEnterRules, action.indent, case-sensitive patterns
#   _enter      editor/common/cursor/cursorTypeEditOperations.ts:582-658
#   onEnter     editor/common/languages/supports/onEnter.ts:52-103
#   getEnterAction
#               editor/common/languages/enterAction.ts:13-60
#   getIndentForEnter
#               editor/common/languages/autoIndent.ts:304-354
#   getInheritIndentForLine
#               editor/common/languages/autoIndent.ts:75-216
#   getPrecedingValidLine
#               editor/common/languages/autoIndent.ts:40-61
#   getIndentationAtPosition
#               editor/common/languages/languageConfigurationRegistry.ts:178-185
#   shiftIndent / unshiftIndent
#               editor/common/linesOperations/browser/shiftCommand.ts,
#               applied as cursorTypeEditOperations.ts:1091-1099 does
#
# Known simplifications, all of which are inert for this corpus:
#   * `IndentationLineProcessor` strips brackets that sit inside strings and
#     comments before the indent patterns are tested. No line in the corpus
#     below puts a bracket inside a string or a comment.
#   * Mixed-language lines (`isLanguageDifferentFromLineStart`) are assumed
#     absent, as they are for a single-language document.
#   * `tabSize` 4, `insertSpaces` true, `indentSize` 4 -- the settings this
#     extension declares.
#   * `editor.autoIndent` is "full" (EditorAutoIndentStrategy.Full = 4).
#   * Multi-cursor edits are not modelled; one selection, keepPosition false.
#
# Usage:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\enter-indent-sim.ps1
param(
    [string]$NxConfig = "editors/vscode-nexum/language-configuration.json",
    [string]$PythonConfig = "tools/vscode-python-language-configuration.json"
)

$ErrorActionPreference = "Stop"

# --- EditorAutoIndentStrategy (editor/common/config/editorOptions.ts:42-48)
$script:AI_NONE = 0
$script:AI_KEEP = 1
$script:AI_BRACKETS = 2
$script:AI_ADVANCED = 3
$script:AI_FULL = 4
$script:AUTO_INDENT = $script:AI_FULL

# --- IndentAction (editor/common/languages/languageConfiguration.ts)
$script:IA_NONE = 0
$script:IA_INDENT = 1
$script:IA_INDENT_OUTDENT = 2
$script:IA_OUTDENT = 3

$script:TAB_SIZE = 4
$script:INDENT_SIZE = 4

# --- IndentConsts (editor/common/languages/supports/indentRules.ts:8-14)
$script:INCREASE_MASK = 1
$script:DECREASE_MASK = 2
$script:INDENT_NEXTLINE_MASK = 4
$script:UNINDENT_MASK = 8

$script:Failures = 0

function Test-Regex([string]$text, [string]$pattern) {
    # VS Code compiles language-configuration patterns with new RegExp(value, '')
    # (languageConfigurationExtensionPoint.ts:446): no flags, so case-sensitive.
    if ([string]::IsNullOrEmpty($pattern)) { return $false }
    return [regex]::IsMatch($text, $pattern, [System.Text.RegularExpressions.RegexOptions]::None)
}

function Get-Leading([string]$line) {
    # strings.getLeadingWhitespace
    $m = [regex]::Match($line, '^[ \t]*')
    return $m.Value
}

function Get-Normalized([string]$s) {
    # model.normalizeIndentation for insertSpaces: tabs in the leading
    # whitespace advance to the next multiple of tabSize. Visual column and
    # character index are different things once a tab is involved, so both
    # are tracked.
    $col = 0
    $idx = 0
    foreach ($ch in $s.ToCharArray()) {
        if ($ch -eq ' ') { $col++ }
        elseif ($ch -eq "`t") { $col = [math]::Floor($col / $script:TAB_SIZE + 1) * $script:TAB_SIZE }
        else { break }
        $idx++
    }
    return (" " * $col) + $s.Substring($idx)
}

function Get-Shifted([string]$indent) {
    # cursorTypeEditOperations.ts:1091-1094 -- `count = count || 1`, so
    # ShiftCommand.shiftIndent(indentation, indentation.length + 1, 4, 4, true)
    # (shiftCommand.ts:60-75) rounds the indentation's visible column up to the
    # next indent tab stop. For space indentation that is "one level deeper".
    $col = (Get-Normalized $indent).Length
    $stop = $col + $script:INDENT_SIZE - ($col % $script:INDENT_SIZE)
    return " " * $stop
}

function Get-Unshifted([string]$indent) {
    # cursorTypeEditOperations.ts:1096-1099 -- ShiftCommand.unshiftIndent with
    # the same +1 column, which lands on prevIndentTabStop
    # (shiftCommand.ts:138-140: max(0, column - 1 - (column - 1) % tabSize)).
    $col = (Get-Normalized $indent).Length
    $stop = [math]::Max(0, $col - 1 - (($col - 1) % $script:INDENT_SIZE))
    return " " * $stop
}

# --- schema: onEnterRules (languageConfigurationExtensionPoint.ts:336-395)
function Read-Config([string]$path) {
    $raw = Get-Content $path -Raw
    $cfg = $raw | ConvertFrom-Json
    $eng = [ordered]@{
        Name          = Split-Path -Leaf $path
        Path          = $path
        Rules         = @()
        Brackets      = @()
        HasIndentRules = $false
        Increase      = $null
        Decrease      = $null
        IndentNext    = $null
        UnIndented    = $null
        Dropped       = @()
    }
    if ($cfg.PSObject.Properties.Name -contains "brackets") {
        foreach ($b in @($cfg.brackets)) {
            $o = $b[0]; $c = $b[1]
            $eng.Brackets += ,@($o, $c)
        }
    }
    if ($null -ne $cfg.indentationRules) {
        $r = $cfg.indentationRules
        $eng.HasIndentRules = $true
        $eng.Increase = $r.increaseIndentPattern
        $eng.Decrease = $r.decreaseIndentPattern
        $eng.IndentNext = $r.indentNextLinePattern
        $eng.UnIndented = $r.unIndentedLinePattern
    }
    if ($null -ne $cfg.onEnterRules) {
        $i = 0
        foreach ($rule in @($cfg.onEnterRules)) {
            $action = $rule.action
            $kind = $script:IA_NONE
            $ok = $false
            if ($null -ne $action -and $action.PSObject.Properties.Name -contains "indent") {
                switch ($action.indent) {
                    "none" { $kind = $script:IA_NONE; $ok = $true }
                    "indent" { $kind = $script:IA_INDENT; $ok = $true }
                    "indentOutdent" { $kind = $script:IA_INDENT_OUTDENT; $ok = $true }
                    "outdent" { $kind = $script:IA_OUTDENT; $ok = $true }
                }
            }
            if (-not $ok) {
                # This is the branch that silently ate this repo's rules.
                $keys = if ($null -ne $action) { ($action.PSObject.Properties.Name -join ",") } else { "" }
                $eng.Dropped += "onEnterRules[$i] (action keys: '$keys')"
                $i++
                continue
            }
            $eng.Rules += [pscustomobject]@{
                Before   = $rule.beforeText
                After    = $rule.afterText
                Previous = $rule.previousLineText
                Kind     = $kind
            }
            $i++
        }
    }
    return [pscustomobject]$eng
}

# --- onEnter (supports/onEnter.ts:52-103)
function Get-EnterAction($eng, [string[]]$doc, [int]$line, [int]$col) {
    $text = $doc[$line - 1]
    $before = if ($col - 1 -le $text.Length) { $text.Substring(0, $col - 1) } else { $text }
    $after = if ($col - 1 -le $text.Length) { $text.Substring($col - 1) } else { "" }
    $prev = if ($line -gt 1) { $doc[$line - 2] } else { "" }

    if ($script:AUTO_INDENT -ge $script:AI_ADVANCED) {
        foreach ($r in $eng.Rules) {
            $hit = $true
            if ($r.Before) { if (-not (Test-Regex $before $r.Before)) { $hit = $false } }
            if ($hit -and $r.After) { if (-not (Test-Regex $after $r.After)) { $hit = $false } }
            if ($hit -and $r.Previous) { if (-not (Test-Regex $prev $r.Previous)) { $hit = $false } }
            if ($hit) { return [pscustomobject]@{ Kind = $r.Kind; AppendText = "" } }
        }
    }
    if ($script:AUTO_INDENT -ge $script:AI_BRACKETS) {
        if ($before.Length -gt 0 -and $after.Length -gt 0) {
            foreach ($b in $eng.Brackets) {
                # _createOpenBracketRegExp / _createCloseBracketRegExp
                # (onEnter.ts:105-127). `{`, `[`, `(` are not word characters,
                # so no \b is added.
                $openPat = [regex]::Escape($b[0]) + '\s*$'
                $closePat = '^\s*' + [regex]::Escape($b[1])
                if ((Test-Regex $before $openPat) -and (Test-Regex $after $closePat)) {
                    return [pscustomobject]@{ Kind = $script:IA_INDENT_OUTDENT; AppendText = "" }
                }
            }
        }
    }
    if ($script:AUTO_INDENT -ge $script:AI_BRACKETS) {
        if ($before.Length -gt 0) {
            foreach ($b in $eng.Brackets) {
                $openPat = [regex]::Escape($b[0]) + '\s*$'
                if (Test-Regex $before $openPat) {
                    return [pscustomobject]@{ Kind = $script:IA_INDENT; AppendText = "" }
                }
            }
        }
    }
    return $null
}

function Should-Increase($eng, [string]$text) { return (Test-Regex $text $eng.Increase) }
function Should-Decrease($eng, [string]$text) { return (Test-Regex $text $eng.Decrease) }
function Should-IndentNext($eng, [string]$text) { return (Test-Regex $text $eng.IndentNext) }
function Should-Ignore($eng, [string]$text) { return (Test-Regex $text $eng.UnIndented) }

function Get-IndentMetadata($eng, [string]$text) {
    $m = 0
    if (Should-Increase $eng $text) { $m = $m -bor $script:INCREASE_MASK }
    if (Should-Decrease $eng $text) { $m = $m -bor $script:DECREASE_MASK }
    if (Should-IndentNext $eng $text) { $m = $m -bor $script:INDENT_NEXTLINE_MASK }
    if (Should-Ignore $eng $text) { $m = $m -bor $script:UNINDENT_MASK }
    return $m
}

# --- getPrecedingValidLine (autoIndent.ts:40-61)
function Get-PrecedingValidLine($eng, [string[]]$doc, [int]$lineNumber) {
    if ($lineNumber -le 1) { return -1 }
    $result = -1
    for ($l = $lineNumber - 1; $l -ge 1; $l--) {
        $text = $doc[$l - 1]
        if ((Should-Ignore $eng $text) -or ($text -match '^[ \t]+$') -or ($text -eq '')) {
            $result = $l
            continue
        }
        return $l
    }
    return -1
}

# --- getInheritIndentForLine (autoIndent.ts:75-216)
# Returns $null, or a hashtable with `indentation` and `action`.
function Get-InheritIndent($eng, [string[]]$doc, [int]$lineNumber, [bool]$honorIntentional) {
    if ($script:AUTO_INDENT -lt $script:AI_FULL) { return $null }
    if (-not $eng.HasIndentRules) { return $null }
    if ($lineNumber -le 1) { return @{ indentation = ""; action = $null } }
    for ($prior = $lineNumber - 1; $prior -gt 0; $prior--) {
        if ($doc[$prior - 1] -ne '') { break }
        if ($prior -eq 1) { return @{ indentation = ""; action = $null } }
    }
    $p = Get-PrecedingValidLine $eng $doc $lineNumber
    if ($p -lt 0) { return $null }
    if ($p -lt 1) { return @{ indentation = ""; action = $null } }
    if ((Should-Increase $eng $doc[$p - 1]) -or (Should-IndentNext $eng $doc[$p - 1])) {
        return @{ indentation = (Get-Leading $doc[$p - 1]); action = $script:IA_INDENT }
    }
    if (Should-Decrease $eng $doc[$p - 1]) {
        return @{ indentation = (Get-Leading $doc[$p - 1]); action = $null }
    }
    if ($p -eq 1) { return @{ indentation = (Get-Leading $doc[0]); action = $null } }
    $previousLine = $p - 1
    $md = Get-IndentMetadata $eng $doc[$previousLine - 1]
    if (($md -band ($script:INCREASE_MASK -bor $script:DECREASE_MASK)) -eq 0 -and
        ($md -band $script:INDENT_NEXTLINE_MASK) -ne 0) {
        $stopLine = 0
        for ($i = $previousLine - 1; $i -gt 0; $i--) {
            if (Should-IndentNext $eng $doc[$i - 1]) { continue }
            $stopLine = $i
            break
        }
        return @{ indentation = (Get-Leading $doc[$stopLine]); action = $null }
    }
    if ($honorIntentional) {
        return @{ indentation = (Get-Leading $doc[$p - 1]); action = $null }
    }
    for ($i = $p; $i -gt 0; $i--) {
        if (Should-Increase $eng $doc[$i - 1]) {
            return @{ indentation = (Get-Leading $doc[$i - 1]); action = $script:IA_INDENT }
        }
        if (Should-IndentNext $eng $doc[$i - 1]) {
            $stopLine = 0
            for ($j = $i - 1; $j -gt 0; $j--) {
                if (Should-IndentNext $eng $doc[$i - 1]) { continue }
                $stopLine = $j
                break
            }
            return @{ indentation = (Get-Leading $doc[$stopLine]); action = $null }
        }
        if (Should-Decrease $eng $doc[$i - 1]) {
            return @{ indentation = (Get-Leading $doc[$i - 1]); action = $null }
        }
    }
    return @{ indentation = (Get-Leading $doc[0]); action = $null }
}

# --- getIndentForEnter (autoIndent.ts:304-354)
function Get-IndentForEnter($eng, [string[]]$doc, [int]$line, [int]$col) {
    if ($script:AUTO_INDENT -lt $script:AI_FULL) { return $null }
    if (-not $eng.HasIndentRules) { return $null }
    $text = $doc[$line - 1]
    $before = if ($col - 1 -le $text.Length) { $text.Substring(0, $col - 1) } else { $text }
    $beforeEnterIndent = Get-Leading $before
    $currentLineIndent = Get-Leading $text
    $afterEnterAction = Get-InheritIndent $eng $doc ($line + 1) $false
    if ($null -eq $afterEnterAction) {
        return @{ beforeEnter = $beforeEnterIndent; afterEnter = $beforeEnterIndent }
    }
    $afterEnterIndent = $afterEnterAction.indentation
    if ($afterEnterAction.action -eq $script:IA_INDENT) { $afterEnterIndent = Get-Shifted $afterEnterIndent }
    if (Should-Decrease $eng $text) { $afterEnterIndent = Get-Unshifted $afterEnterIndent }
    return @{ beforeEnter = $beforeEnterIndent; afterEnter = $afterEnterIndent }
}

# --- getIndentationAtPosition (languageConfigurationRegistry.ts:178-185)
function Get-IndentAtPosition([string[]]$doc, [int]$line, [int]$col) {
    $indentation = Get-Leading $doc[$line - 1]
    if ($indentation.Length -gt ($col - 1)) { $indentation = $indentation.Substring(0, $col - 1) }
    return $indentation
}

# --- _enter (cursorTypeEditOperations.ts:582-658)
# Returns the indentation of the first newly inserted line, and the action.
function Get-EnterResult($eng, [string[]]$doc, [int]$line, [int]$col) {
    if ($script:AUTO_INDENT -eq $script:AI_NONE) { return @{ indent = ""; action = "none" } }
    if ($script:AUTO_INDENT -eq $script:AI_KEEP) {
        $text = $doc[$line - 1]
        $ws = Get-Leading $text
        if ($ws.Length -gt ($col - 1)) { $ws = $ws.Substring(0, $col - 1) }
        return @{ indent = (Get-Normalized $ws); action = "keep" }
    }
    $r = Get-EnterAction $eng $doc $line $col
    if ($null -ne $r) {
        # enterAction.ts:41-56 -- an absent appendText becomes a single tab for
        # Indent/IndentOutdent and the empty string otherwise.
        $append = $r.AppendText
        if ([string]::IsNullOrEmpty($append)) {
            $append = if ($r.Kind -eq $script:IA_INDENT -or $r.Kind -eq $script:IA_INDENT_OUTDENT) { "`t" } else { "" }
        }
        elseif ($r.Kind -eq $script:IA_INDENT) { $append = "`t" + $append }
        $indentation = Get-IndentAtPosition $doc $line $col
        switch ($r.Kind) {
            $script:IA_NONE {
                return @{ indent = (Get-Normalized ($indentation + $append)); action = "none" }
            }
            $script:IA_INDENT {
                return @{ indent = (Get-Normalized ($indentation + $append)); action = "indent" }
            }
            $script:IA_INDENT_OUTDENT {
                return @{ indent = (Get-Normalized ($indentation + $append)); action = "indentOutdent" }
            }
            $script:IA_OUTDENT {
                return @{ indent = (Get-Normalized ((Get-Unshifted $indentation) + $append)); action = "outdent" }
            }
        }
    }
    $text = $doc[$line - 1]
    $ws = Get-Leading $text
    if ($ws.Length -gt ($col - 1)) { $ws = $ws.Substring(0, $col - 1) }
    $indentation = $ws
    if ($script:AUTO_INDENT -ge $script:AI_FULL) {
        $ir = Get-IndentForEnter $eng $doc $line $col
        if ($null -ne $ir) {
            return @{ indent = (Get-Normalized $ir.afterEnter); action = "rules" }
        }
    }
    return @{ indent = (Get-Normalized $indentation); action = "fallback" }
}

# ===========================================================================
# Corpus
# ===========================================================================
#
# The differential is only meaningful if the document is valid in BOTH
# languages, so the shared cases use the common subset: `if`/`else`/`while`,
# indexing, arithmetic, calls. `fn` has no Python spelling and `def` no NX
# one, so cases that need a function are marked NX-only and their expected
# column is asserted without a Python comparison.
#
# $cases rows: name, document, line, column, expected NX column, comparable
#   comparable = $true  -> NX and Python must produce the same column
#   comparable = $false -> NX-only; the expected column still has to hold

# Shared: two levels of nesting, an else, and a statement after the block.
$common = @(
    "xs = [3, 2, 1]",
    "i = 1",
    "if i > 0:",
    "    while xs[i - 1] > xs[i]:",
    "        i = i + 1",
    "    if i > 5:",
    "        print(i)",
    "    else:",
    "        print(0)",
    "print(i)"
)

# Shared: the reported shape. A blank line at column 0 after leaving a block.
$blanks = @(
    "if i > 0:",
    "    print(1)",
    "",
    "print(i)"
)

# Shared: a blank line that still carries the block's indentation.
$carries = @(
    "if i > 0:",
    "    print(1)",
    "    "
)

# Shared: a bracket pair left open.
$brackets = @(
    "xs = [",
    "    1,",
    "    2",
    "]"
)

# NX-only: a function, and a block opener with a trailing comment.
$nxFn = @(
    "fn fib(n):",
    "    return n",
    ""
)
$nxComment = @(
    "fn f():  # start",
    "    x = 1",
    ""
)

$cases = @(
    # -- the report, and its neighbours
    @("blank line at col 0 after a block (the report)", $blanks, 3, 1, 0, $true),
    @("blank line at col 0 after print()", $blanks, 4, 1, 0, $true),
    @("blank line carrying the block indent keeps it", $carries, 3, 5, 4, $true),

    # -- block openers, shared subset
    @("end of 'if i > 0:'", $common, 3, 11, 4, $true),
    @("end of '    while xs[i - 1] > xs[i]:'", $common, 4, 30, 8, $true),
    @("end of '        i = i + 1' (2 levels deep)", $common, 5, 19, 8, $true),
    @("end of '    if i > 5:'", $common, 6, 15, 8, $true),
    @("end of '        print(i)' inside the if", $common, 7, 18, 8, $true),
    @("end of '    else:'", $common, 8, 10, 8, $true),
    @("end of '        print(0)' inside the else", $common, 9, 18, 8, $true),
    @("col 0 of 'print(i)' after the block", $common, 10, 1, 0, $true),
    @("col 0 of '    else:' takes the line own indent", $common, 8, 1, 0, $true),

    # -- brackets, shared subset
    @("end of '    2' inside brackets", $brackets, 3, 6, 4, $true),
    @("col 0 of ']'", $brackets, 4, 1, 0, $true),

    # -- NX-only
    @("end of 'fn fib(n):'", $nxFn, 1, 11, 4, $false),
    @("end of '    return n' in a function", $nxFn, 2, 14, 4, $false),
    @("blank line at col 0 after a function", $nxFn, 3, 1, 0, $false),
    @("end of 'fn f():  # start' (trailing comment)", $nxComment, 1, 18, 4, $false),
    @("blank line at col 0 after a commented opener", $nxComment, 3, 1, 0, $false)
)

# Differences from Python that are deliberate. Each needs a reason, because
# an unlisted difference is a bug and a listed one is a decision.
$differReason = @{
    "end of 'fn fib(n):'"                            = "NX-only: `fn` has no Python spelling."
    "end of '    return n' in a function"            = "NX-only: `fn` has no Python spelling."
    "blank line at col 0 after a function"           = "NX-only: `fn` has no Python spelling."
    "end of 'fn f():  # start' (trailing comment)"   = "NX-only: `fn` has no Python spelling. NX's opener pattern also tolerates a trailing comment, which Python's does not -- `def f():  # x` does not end in `:` there, so Python does not indent it either."
    "blank line at col 0 after a commented opener"   = "NX-only: `fn` has no Python spelling."
}

# ===========================================================================
# Run
# ===========================================================================

$nx = Read-Config $NxConfig
$py = Read-Config $PythonConfig

"config under test : $($nx.Path)"
"  onEnterRules kept: $($nx.Rules.Count)   dropped by VS Code's schema: $($nx.Dropped.Count)"
foreach ($d in $nx.Dropped) { "    DROPPED $d" }
"  indentationRules: $(if ($nx.HasIndentRules) { 'present' } else { 'absent (Enter keeps each line own indent)' })"
"reference         : $($py.Path)  (VS Code's shipped Python configuration)"
"  onEnterRules kept: $($py.Rules.Count)   dropped: $($py.Dropped.Count)"
""

$header = "{0,-4} {1,-52} {2,5} {3,5}  {4}" -f "", "case", "nx", "py", "vs python"
"{0,-4} {1,-52} {2,5} {3,5}  {4}" -f "", ("-" * 52), "----", "----", "---------"

foreach ($c in $cases) {
    $name = $c[0]; $doc = $c[1]; $line = [int]$c[2]; $col = [int]$c[3]
    $want = [int]$c[4]; $comparable = [bool]$c[5]
    $rn = Get-EnterResult $nx $doc $line $col
    $gn = $rn.indent.Length
    $gp = $null
    $mark = "n/a"
    $ok = $true
    $why = @()

    if ($gn -ne $want) { $ok = $false; $why += "expected col $want" }

    if ($comparable) {
        $rp = Get-EnterResult $py $doc $line $col
        $gp = $rp.indent.Length
        if ($gn -ne $gp) { $ok = $false; $why += "python gives col $gp" }
        $mark = if ($gn -eq $gp) { "same" } else { "DIFFER" }
    }
    else {
        if ($differReason.ContainsKey($name)) { $mark = "nx-only" }
        else { $ok = $false; $why += "not comparable with Python but no reason is recorded" }
    }

    $tag = if ($ok) { "ok" } else { "FAIL" }
    if (-not $ok) { $script:Failures++ }
    "{0,-4} {1,-52} {2,5} {3,5}  {4}" -f $tag, $name, $gn, $(if ($null -eq $gp) { "-" } else { $gp }), $mark
    if ($why.Count -gt 0) { "       -> $($why -join '; ')" }
    if ($mark -eq "nx-only") { "       note: $($differReason[$name])" }
}

""

# --- self-tests: this harness must be able to go red ------------------------
# A gate that cannot fail is not a gate. Each case below rebuilds the config
# deliberately broken, re-runs the corpus, and requires the breakage to show
# up at a *named* case with a *named* indentation -- not merely that something
# moved. These are the three mistakes this file exists to prevent.

"self-tests (the harness must be able to go red)"

function Get-Case([string]$name) {
    foreach ($c in $cases) { if ($c[0] -eq $name) { return $c } }
    throw "self-test names a case that does not exist: $name"
}

# $expectRows: @(caseName, expected indent string under the broken config)
function Test-Red([string]$name, [scriptblock]$mutate, $expectRows, [string]$expectDropped) {
    $cfg = (Get-Content $NxConfig -Raw) | ConvertFrom-Json
    $brokenCfg = & $mutate $cfg
    $tmp = [System.IO.Path]::GetTempFileName()
    try {
        [System.IO.File]::WriteAllText($tmp, ($brokenCfg | ConvertTo-Json -Depth 10))
        $broken = Read-Config $tmp
        $problems = @()
        foreach ($row in $expectRows) {
            $c = Get-Case $row[0]
            $doc = $c[1]; $line = [int]$c[2]; $col = [int]$c[3]
            $got = (Get-EnterResult $broken $doc $line $col).indent
            if ($got -ne $row[1]) {
                $problems += "'$($row[0])' became col $($got.Length), expected col $($row[1].Length)"
            }
        }
        $dropped = ($broken.Dropped -join "; ")
        if ($expectDropped -and $dropped -notmatch $expectDropped) {
            $problems += "the schema did not report a dropped rule (got: '$dropped')"
        }
        if ($problems.Count -gt 0) {
            Write-Host ("FAIL {0}" -f $name)
            foreach ($p in $problems) { Write-Host ("       {0}" -f $p) }
            $script:Failures++
        }
        else {
            $how = @("$($expectRows.Count) named case(s) broke as required")
            if ($dropped) { $how += "schema reported: $dropped" }
            Write-Host ("ok   {0}: {1}" -f $name, ($how -join '; '))
        }
    }
    finally { Remove-Item $tmp -Force -ErrorAction SilentlyContinue }
}

# 1. The bug that shipped: `action.indentAction` instead of `action.indent`.
#    VS Code drops both rules with a console warning, so nothing in the config
#    is left to indent a block opener, and nothing keeps a blank line at
#    column 0.
Test-Red "action.indentAction is rejected by the schema" {
    param($cfg)
    foreach ($r in $cfg.onEnterRules) {
        $r.action = [pscustomobject]@{ indentAction = $r.action.indent }
    }
    $cfg
} @(
    @("end of 'if i > 0:'", ""),
    @("end of 'fn fib(n):'", ""),
    @("blank line at col 0 after a block (the report)", "")
) "onEnterRules"

# 2. The configuration this repository shipped until v0.4.5: it carried
#    `indentationRules`, which replaces "keep this line's own indent" with
#    "recompute from the nearest preceding line that matches a rule". With no
#    rule matching, VS Code's walk gives up and returns line 1's indentation.
#    Two failures follow, and both are named here: the reported one, and a
#    second one nobody had reported yet -- continuing a list literal.
Test-Red "indentationRules without a blank-line rule reproduces the report" {
    param($cfg)
    $cfg | Add-Member -NotePropertyName indentationRules -NotePropertyValue ([pscustomobject]@{
            increaseIndentPattern = '^\s*(?:fn|type|impl|if|elif|else|for|while)\b.*:\s*(#.*)?$'
            decreaseIndentPattern = '^\s*(elif|else)\b.*:\s*(#.*)?$'
        })
    $cfg.onEnterRules = @($cfg.onEnterRules | Where-Object { $_.beforeText -ne '^\s*$' })
    $cfg
} @(
    @("blank line at col 0 after a block (the report)", "    "),
    @("end of '    2' inside brackets", "")
) ""

# 3. The same `indentationRules` with the blank-line rule restored: the report
#    goes away but the list-literal case does not, which is why the rules were
#    removed rather than patched.
Test-Red "indentationRules alone still breaks list literals" {
    param($cfg)
    $cfg | Add-Member -NotePropertyName indentationRules -NotePropertyValue ([pscustomobject]@{
            increaseIndentPattern = '^\s*(?:fn|type|impl|if|elif|else|for|while)\b.*:\s*(#.*)?$'
            decreaseIndentPattern = '^\s*(elif|else)\b.*:\s*(#.*)?$'
        })
    $cfg
} @(
    @("blank line at col 0 after a block (the report)", ""),
    @("end of '    2' inside brackets", "")
) ""

# 4. An opener pattern that does not tolerate a trailing comment: `fn f():  # x`
#    stops indenting, which is the one thing the NX pattern is better at than
#    Python's.
Test-Red "opener pattern without comment tolerance" {
    param($cfg)
    $bare = '^\s*(?:fn|type|impl|if|elif|else|for|while)\b.*:\s*$'
    $cfg.onEnterRules[0].beforeText = $bare
    $cfg
} @(
    @("end of 'fn f():  # start' (trailing comment)", ""),
    @("end of 'fn fib(n):'", "    ")
) ""

""
if ($script:Failures -gt 0) {
    Write-Host ("ENTER-INDENT SIM: {0} FAILURE(S)" -f $script:Failures)
    exit 1
}
Write-Host "enter-indent sim: all cases match the ported VS Code pipeline"
exit 0
