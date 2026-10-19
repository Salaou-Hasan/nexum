# The compiler pipeline, stage by stage

**Question.** What does the path from a `.nx` file to a binary look like today,
stage by stage: what each stage's input and output types are, which files it
owns, which functions are the extension points a new language feature has to
touch, and which other stages it depends on? The answer is meant to be a map
that lets the orchestrator cut phases 11b to 11f (inline assembly, the numeric
tower, memory and layout, concurrency, SIMD) into parallel, non-overlapping
slices of work.

**Summary.** The pipeline is nine Rust crates in a strict dependency order —
`nexum-source` → `nexum-lexer` → `nexum-parser` → `nexum-sema` → `nexum-mir` →
`nexum-codegen` (+ `nexum-runtime`, `nexum-target`) → `nexum-toolchain` — driven
by `nexum-driver`. The stages are already separated by crate, so a language
feature can be cut by stage: one agent owns the front end of it (lexer, parser,
sema), one owns the back end of it (MIR, codegen, runtime), one owns the
runtime primitive, and one owns the tests and the docs. The recommendation is
a stage-by-stage table of extension points plus an exact, ordered recipe for
"how to add a language feature", because the ordering is not free: the type
representation (`nexum_sema::Ty`) is *shared* by MIR and codegen, and the
`Instr` enum is matched exhaustively in five places, so a new MIR instruction
cannot be added without touching the optimizer at the same time. The one thing
that will break other people's work if it is ignored is the self-hosting gate:
`crates/nexum-driver/tests/backend.rs:142` and `:198` compare the self-hosted
lexer and parser against the Rust ones, so a new token kind or a new syntactic
form is a change to `examples/selfhost/lexer_lib.nx` and
`examples/selfhost/parser.nx` as well, not just to the Rust crates.

---

## Recommendation

### 1. The pipeline, in one picture

```
   .nx file(s)
        │
        │  nexum_parser::load_module        crates/nexum-parser/src/lib.rs:196
        │    └─ per file: nexum_lexer::lex  crates/nexum-lexer/src/lib.rs:137
        │                 └─ nexum_parser::parse  crates/nexum-parser/src/lib.rs:174
        ▼
   Module (AST)  ──  nexum_parser::normalize::normalize  crates/nexum-parser/src/normalize.rs:248
        │
        │  nexum_sema::analyze              crates/nexum-sema/src/lib.rs:208
        ▼
   HirModule  (Option<HirModule>; None if any error diagnostic)  sema:428
        │
        │  nexum_mir::lower                 crates/nexum-mir/src/lib.rs:182
        │  nexum_mir::optimize_level        crates/nexum-mir/src/lib.rs:1383
        │  nexum_mir::verify                crates/nexum-mir/src/lib.rs:772
        ▼
   MirModule                               crates/nexum-mir/src/lib.rs:171
        │
        │  nexum_codegen::emit              crates/nexum-codegen/src/lib.rs:109
        │    └─ nexum_runtime::runtime_ir   crates/nexum-runtime/src/lib.rs:46
        ▼
   LLVM IR text (a String)
        │
        │  nexum_toolchain::verify_ir_for   crates/nexum-toolchain/src/lib.rs:221
        │  nexum_toolchain::run_clang_with  crates/nexum-toolchain/src/lib.rs:178
        ▼
   a native binary
```

The driver holds the whole path in one function, `compile_to_mir`
(`crates/nexum-driver/src/main.rs:1039`), and every compiling command goes
through it: `check_file` (`:1121`), `build_file` (`:1128`), `run_file` (`:1153`)
→ `build_to` (`:1160`), `Analyze` (`:152`), `EmitIr` (`:195`). A new command
that compiles something should call `compile_to_mir`, not re-implement the path.

### 2. Stage-by-stage table: the extension point for each kind of change

The "extension point" column names the *exact* function and line a change has
to land in. Everything else in that stage is either stable or derived.

| # | Stage | Owns (files) | Input → output | Extension point for a change |
|---|-------|---------------|----------------|------------------------------|
| 0 | **Driver** | `crates/nexum-driver/src/main.rs`, `src/setup.rs` | `Path` → exit code / artefact | `compile_to_mir` `main.rs:1039` (the one shared compile path); `Cli` `main.rs:15`; `Commands` `main.rs:27`; `explain` `main.rs:947` (the T-code table); `format_nx` `main.rs:522` (a second, hand-written AST walk — breaks on any new node); `current_target` `main.rs:1030` |
| 1 | **Lexer** | `crates/nexum-lexer/src/lib.rs` | `&SourceFile` → `LexerOutput { tokens, diagnostics }` | `TokenKind` `:7` (the enum); `TokenKind::name()` `:68` (**the string is a contract with the self-hosted lexer**); keyword table `:342`; two-char operator table `:370`; single-char table `:390`; number scanner `:304`; string scanner `:261`; indent/dedent `:188` |
| 2 | **Parser** | `crates/nexum-parser/src/lib.rs`, `src/normalize.rs` | `&[Token]` → `ParseResult { module, diagnostics }` | `TypeExpr` `:13`; `ExprKind` `:48`; `StmtKind` `:102`; `ForIter` `:157`; `PathSeg` `:81`; `parse_stmt` `:336` (statement dispatch); `parse_type_expr` `:648`; the precedence chain `parse_or` `:875` → `parse_and` `:892` → `parse_eq` `:909` → `parse_cmp` `:931` → `parse_add` `:955` → `parse_mul` `:977` → `parse_unary` `:1000` → `parse_postfix` `:1029` → `parse_primary` `:1077`; `load_file` `:204` (imports) |
| 2b | **Normalised form** | `crates/nexum-parser/src/normalize.rs` | `&Module` → `String` | `type_expr` `:54`; `expr` `:66`; `stmt` `:107`; `binop_name` `:14`; `unop_name` `:32`. **The line prefixes are the contract with the self-hosted parser** |
| 3 | **Sema** | `crates/nexum-sema/src/lib.rs` | `&AModule` → `SemaResult { hir: Option<HirModule>, diagnostics }` | `Ty` `:15` (**the shared type representation**); `Ty::name` `:35`; `Ty::compatible` `:54`; `HirExpr` `:124`; `HirStmt` `:91`; `tty` `:537` (the primitive-type name table); `ty_check` `:559`; `analyze_block` `:599`; `analyze_expr` `:903` (the **builtin dispatch**, one `if callee == "…"` block per builtin, `:1128`–`:1637`); `analyze_path_assign` `:1755`; `binop_type` `:1965`; `Scope` `:466` + `merge_refined` `:498`; `Ctx` `:447` |
| 4 | **MIR** | `crates/nexum-mir/src/lib.rs` | `&HirModule` → `LowerResult { module, diagnostics }` | `Const` `:16`; `BinOp` `:24`; `UnOp` `:41`; `Instr` `:47` (**new instructions land here**); `Term` `:138`; `MirFn` `:157`; `MirModule` `:171`; `lower` `:182`; `lower_fn` `:241`; `lower_stmt` `:335`; `lower_expr` `:527`; `verify` `:772`; the optimizer: `const_fold` `:792` + `fold_binop` `:867` + `fold_unop` `:913`, `dce` `:926` + `collect_uses` `:993` + `instr_dest` `:1053` + `instr_ty` `:1072` + `is_side_effecting` `:1091`, `inline_small` `:1100` + `remap_inputs` `:1230` + `set_instr_dest` `:1294`; `optimize_level` `:1383` |
| 5 | **Codegen** | `crates/nexum-codegen/src/lib.rs` | `&MirModule` + `&TargetSpec` → LLVM IR text | `llvm_ty` `:8` (**the type mapping**); `has_refs` `:23` (**the collector predicate**); `llvm_align` `:36`; `llvm_size` `:55`; `record_size` `:70`; `scan_bytes` `:85`; `list_elem_ty` `:102`; `emit` `:109`; `emit_fn` `:228`; the **memory plan** `:281`–`:351`; the **root entries** `:361`–`:389`; the prologue `:396`–`:458`; `Instr::Call` `:691` (the runtime symbol table `:836`–`:847`); `print` `:716`; `list_push`/`list_set` `:756`; `list_get` `:802`; the terminator `:1176` (`nexum_roots_pop` `:1193`) |
| 6 | **Runtime** | `crates/nexum-runtime/src/lib.rs`, `src/strings.rs`, `src/syscalls.rs` | `(os, arch)` → LLVM IR text (spliced into every module) | `runtime_ir` `lib.rs:46`; the collector `lib.rs:142`–`:187`; the strings `lib.rs:189`–`:202`; the lists `lib.rs:208`–`:231`; `strings::string_routines` `strings.rs:11`; `strings::file_routines` `strings.rs:33`; `syscalls::Syscalls::for_os` `syscalls.rs:58`; `syscalls::layout` `syscalls.rs:110`; `syscalls::wrappers` `syscalls.rs:237` |
| 7 | **Target** | `crates/nexum-target/src/lib.rs` | name → `TargetSpec { triple, pointer_width, os, arch }` | `TargetSpec` `:4`; `host` `:14`; `by_name` `:39`; `list` `:75` (**the target list**); `data_layout_str` `:85` |
| 8 | **Toolchain** | `crates/nexum-toolchain/src/lib.rs` | paths → a linked binary | `Toolchain::discover` `:13` (finds `clang`); `BuildOpts` `:161`; `run_clang_with` `:178`; `verify_ir_for` `:221`; `find_exe` `:143` |
| 9 | **Tests** | `crates/nexum-driver/tests/backend.rs`, `tests/fixtures.rs`, `tests/common/mod.rs`, `tests/fixtures/{pass,fail,parse,run}/` | `.nx` files → pass/fail | `fixtures.rs:14` `compile_pass_fixtures`; `fixtures.rs:40` `compile_fail_fixtures` (the `# EXPECT: Txxx` convention `:48`–`:61`); `backend.rs:44` `ir_verifies_for_pass_fixtures`; `backend.rs:52` `ir_verifies_for_examples`; `backend.rs:60` `ir_verifies_for_all_targets`; `backend.rs:76` `codegen_emits_expected_fragments`; `backend.rs:93` `run_outputs_match_fixtures`; `backend.rs:142` `selfhosted_lexer_matches_rust_lexer`; `backend.rs:198` `selfhosted_parser_matches_rust_parser`; `backend.rs:251` `differential_opt_levels_agree` |
| 10 | **Self-hosting gate** | `examples/selfhost/lexer.nx`, `examples/selfhost/lexer_lib.nx`, `examples/selfhost/parser.nx` | `.nx` → the same token stream / normalised form | `lexer_lib.nx:37` `keyword_kind`; `:73` `two_char_kind`; `:98` `single_char_kind`; `:134` `lex_tokens`; `parser.nx:139`–`:351` the expression parsers; `:377`–`:582` the statement parsers; `:638` `print_node` (**the normalised-form printer**); `:766` `main` |

### 3. The table of "what a change of each kind touches"

This is the table the orchestrator should actually cut work along. The right
column is the set of files one agent must own for that kind of change; if two
agents' rows share a file, they cannot be parallel.

| Kind of change | Files it touches (in order) | Owner slice |
|----------------|----------------------------|-------------|
| **New keyword** | `nexum-lexer/src/lib.rs:7` (add the variant) → `:68` (`name()`) → `:342` (keyword table) → `examples/selfhost/lexer_lib.nx:37` (`keyword_kind`) | front end + self-host |
| **New token / operator** | as above, plus the operator tables `nexum-lexer/src/lib.rs:370`/`:390` and `examples/selfhost/lexer_lib.nx:73`/`:98` | front end + self-host |
| **New statement form** | `nexum-parser/src/lib.rs:102` (`StmtKind`) + the dispatch in `:336` → `nexum-parser/src/normalize.rs:107` (`stmt`) → `nexum-sema/src/lib.rs:91` (`HirStmt`) + `analyze_block` `:599` → `nexum-mir/src/lib.rs:91`+`lower_stmt` `:335` → `nexum-codegen/src/lib.rs:228` (`emit_fn`) → `examples/selfhost/parser.nx:600` (`parse_stmt`) + `:638` (`print_node`) → `crates/nexum-driver/src/main.rs:522` (`format_nx`) | **all front-end stages + self-host** — one agent, one feature |
| **New expression form** | `nexum-parser/src/lib.rs:48` (`ExprKind`) + the precedence chain `:875`–`:1160` → `nexum-parser/src/normalize.rs:66` (`expr`) → `nexum-sema/src/lib.rs:124` (`HirExpr`) + `analyze_expr` `:903` → `nexum-mir/src/lib.rs:527` (`lower_expr`) → `nexum-codegen/src/lib.rs:459` (the instruction match) → `examples/selfhost/parser.nx:251` + `:638` → `crates/nexum-driver/src/main.rs:587` (`fmt_expr`) | as above — one agent, one feature |
| **New primitive type** (`f32`, `i32`, …) | `nexum-sema/src/lib.rs:15` (`Ty`) → `:35` (`name`) → `:54` (`compatible`) → `:537` (`tty`, the name table) → `:1965` (`binop_type`) → `nexum-mir/src/lib.rs:16` (`Const`) + `:47` (`Instr`) → `nexum-codegen/src/lib.rs:8` (`llvm_ty`) + `:23` (`has_refs`) + `:36`/`:55`/`:85` (align/size/scan) | **one agent**: the type touches sema, MIR and codegen |
| **New builtin** | `nexum-sema/src/lib.rs:1123`–`:1637` (arity/type checking + result type) → `nexum-codegen/src/lib.rs:836` (the symbol table) or `:716`/`:756`/`:802` (a special lowering) → `nexum-runtime/src/lib.rs:46` (the implementation, if it needs one) → `crates/nexum-driver/src/main.rs:947` (`explain`, if it has a diagnostic) → `docs/standard-library.md` | **one agent** (front end + back end + runtime) |
| **New MIR instruction** | `nexum-mir/src/lib.rs:47` (`Instr`) → `:527` (`lower_expr`) → `:993` (`collect_uses`) → `:1053` (`instr_dest`) → `:1072` (`instr_ty`) → `:1230` (`remap_inputs`) → `:1294` (`set_instr_dest`) → `nexum-codegen/src/lib.rs:459` (the emission match) | **one agent**: MIR + codegen must land together |
| **New diagnostic code** | the stage that raises it → `crates/nexum-driver/src/main.rs:947` (`explain`) → `docs/language-reference/grammar.md` §5 | the raising stage + docs |
| **New runtime primitive** | `nexum-runtime/src/lib.rs:46` → `crates/nexum-codegen/src/lib.rs:836` (the symbol) → `docs/runtime-abi.md` | runtime + codegen |
| **New target** | `nexum-target/src/lib.rs:39` (`by_name`) + `:75` (`list`) + `:85` (`data_layout_str`) → `crates/nexum-runtime/src/lib.rs:60`–`:97` (the OS/arch branch) | target + runtime |
| **New language rule** (not a new syntax) | `docs/language-reference/decisions.md` (a numbered entry) + `docs/language-reference/grammar.md` §4 + `crates/nexum-sema/src/lib.rs` | semantics + docs |

### 4. How to add a language feature, step by step

The order matters, because each step is a prerequisite of the next, and because
the last two steps are the ones people forget. The canonical order for a
feature that adds **syntax** (the 11b inline-assembly case is the model):

1. **Grammar first, in writing.** `docs/language-reference/grammar.md` §2
   (lexical) and §3 (syntactic). If the feature changes a rule rather than
   adding a form, add a numbered entry to
   `docs/language-reference/decisions.md` instead (D014 already reserves
   inline assembly; D015 the numeric tower). The decision entry is what the
   other agents read, so it comes *before* the code, not after.
2. **Lexer** — `crates/nexum-lexer/src/lib.rs`: add the `TokenKind` variant
   (`:7`), give it a stable name in `TokenKind::name()` (`:68`), and add it to
   the keyword table (`:342`) or the operator tables (`:370`, `:390`).
   *A token kind without a `name()` entry is invisible to the self-hosted
   lexer, and the differential test at `backend.rs:142` fails on every example
   program.*
3. **Parser** — `crates/nexum-parser/src/lib.rs`: add the AST node
   (`ExprKind` `:48` / `StmtKind` `:102` / `TypeExpr` `:13` / `ForIter`
   `:157`), and the parse function (a new arm in `parse_stmt` `:336`, or a new
   level in the precedence chain `:875`–`:1160`, or a new arm in
   `parse_primary` `:1077`).
4. **Normalised form** — `crates/nexum-parser/src/normalize.rs`: print the new
   node (`expr` `:66` / `stmt` `:107` / `type_expr` `:54`). The normalised
   form is the *only* thing the self-hosted parser is compared on, so this
   step and step 9 are one contract, not two.
5. **Sema** — `crates/nexum-sema/src/lib.rs`: add the HIR node (`HirExpr`
   `:124` / `HirStmt` `:91`), type it in `analyze_expr` `:903` or
   `analyze_block` `:599`, and give it a type in `Ty` `:15` if it needs a new
   one. A new *builtin* goes in the builtin dispatch in `analyze_expr`
   (`:1123`–`:1637`) as its own `if callee == "…"` block, with its own
   diagnostic code.
6. **MIR** — `crates/nexum-mir/src/lib.rs`: lower the HIR node in
   `lower_expr` `:527` or `lower_stmt` `:335`. If it needs a new
   instruction, add it to `Instr` `:47` **and** to `collect_uses` `:993`,
   `instr_dest` `:1053`, `instr_ty` `:1072`, `remap_inputs` `:1230` and
   `set_instr_dest` `:1294` in the same commit — those five are exhaustive
   matches over `Instr` and the compiler will not build otherwise. Add a
   verifier rule in `verify` `:772` if the instruction has an invariant
   (bounds, alignment) the checker should enforce.
7. **Codegen** — `crates/nexum-codegen/src/lib.rs`: emit the instruction in
   `emit_fn` `:228`'s instruction match (`:459`–`:1172`). If the new value
   type can hold a reference, it must also be added to `has_refs` `:23` and
   to the memory plan `:281`–`:351` and the root entries `:361`–`:389`,
   otherwise the collector will free it under the program. If the feature
   needs a runtime entry point, add the symbol to the table at `:836`–`:847`
   and implement it in `crates/nexum-runtime/src/lib.rs:46`.
8. **Runtime** — `crates/nexum-runtime/src/lib.rs` (or `strings.rs` /
   `syscalls.rs`): the implementation. Then `docs/runtime-abi.md` if the
   layout or the ABI changed.
9. **The self-hosted compiler** — `examples/selfhost/lexer_lib.nx` (if the
   token set changed: `keyword_kind` `:37`, `two_char_kind` `:73`,
   `single_char_kind` `:98`) and `examples/selfhost/parser.nx` (if the
   syntactic form changed: the parse function, plus `print_node` `:638` so
   the normalised form matches byte for byte). *This step is not optional
   and it is not "later": `backend.rs:142` runs the self-hosted lexer over
   every file in `examples/`, and `backend.rs:198` runs the self-hosted
   parser over every file in `tests/fixtures/parse/`.*
10. **Tests** — in this order, because each kind of fixture proves a
    different thing:
    * `crates/nexum-driver/tests/fixtures/pass/<feature>.nx` — must compile
      with zero error diagnostics (`fixtures.rs:14`).
    * `crates/nexum-driver/tests/fixtures/fail/<feature>.nx` — with
      `# EXPECT: Txxx` comments (`fixtures.rs:40`); every diagnostic must
      also carry a span.
    * `crates/nexum-driver/tests/fixtures/parse/<feature>.nx` — if the
      feature added a syntactic form, this is what makes the self-hosted
      parser differential test meaningful.
    * `crates/nexum-driver/tests/fixtures/run/<feature>.nx` + `<feature>.out`
      (+ `<feature>.exit` if the program exits non-zero) — the end-to-end
      behaviour (`backend.rs:93`).
    * `examples/<feature>.nx` — an example is IR-verified
      (`backend.rs:52`) *and* is the input of the self-hosted lexer
      differential test (`backend.rs:166`), so it is the cheapest place to
      cover a feature that must survive the self-hosted lexer.
    * `crates/nexum-driver/tests/backend.rs:76` — extend
      `codegen_emits_expected_fragments` if the feature should produce a
      specific IR fragment (this is where "the arithmetic must be wrapping,
      no `nsw`" is pinned).
11. **Docs** — `docs/language-reference/grammar.md` (§3 grammar, §4 static
    semantics, §5 diagnostic codes), `docs/standard-library.md` (if a
    builtin), `docs/runtime-abi.md` (if the ABI), and
    `crates/nexum-driver/src/main.rs:947` (`explain`, the human-readable
    table of diagnostic codes — a new T-code with no entry here is a code
    `nx explain` cannot describe).
12. **Formatter** — `crates/nexum-driver/src/main.rs:522` (`format_nx`).
    `nx fmt` is a *second* implementation of the AST walk, written by hand
    with exhaustive `match`es over `ExprKind` and `StmtKind`. A new node
    breaks it at compile time, which is the intended alarm, but it means the
    formatter is part of the feature, not an afterthought.

### 5. Slices the orchestrator can actually run in parallel

Because the stages are separate crates, the phases 11b–11f work splits
cleanly *if* the type and the AST are agreed up front. The three files that
would otherwise be contended are:

* `crates/nexum-sema/src/lib.rs` — `Ty` `:15` and the builtin dispatch
  `:1123`. **One agent owns this file.** Everything that adds a type or a
  builtin goes through them.
* `crates/nexum-codegen/src/lib.rs` — `llvm_ty` `:8`, `has_refs` `:23`, the
  memory plan `:281`, the call lowering `:691`. **One agent owns this file.**
* `crates/nexum-driver/src/main.rs` — `explain` `:947` and `format_nx`
  `:522`. **One agent owns this file.**

Everything else (`nexum-lexer`, `nexum-parser` + `normalize.rs`, `nexum-mir`,
`nexum-runtime`, `nexum-target`, `nexum-toolchain`, `examples/selfhost/*`,
the fixture directories and the docs) is a separate file per stage, so one
phase's agent can own one of them outright.

---

## What the code looks like today

Only the facts the recommendation depends on.

### The driver — `crates/nexum-driver/src/main.rs`

* `Cli` (`:15`) carries two global options: `--opt-level` (`:17`, default 0)
  and `--target` (`:20`, default `host`).
* `Commands` (`:27`) has: `Run`, `Check`, `Build`, `EmitIr`, `Update`, `Lex`,
  `Parse`, `New`, `Fmt`, `Test`, `Doctor`, `Setup`, `Explain`, `Analyze`,
  `ExplainToolchain`.
* `main()` (`:94`) sets the global target from `--target` (`:96`) and
  dispatches.
* **The shared compile path** is `compile_to_mir` (`:1039`): it loads the
  module and its imports into one `SourceMap` (`:1045`–`:1055`), runs
  `analyze` (`:1056`), aborts on any error diagnostic (`:1060`–`:1066`),
  takes `sema.hir` (`:1067`), lowers (`:1068`), optimises at the requested
  level (`:1070`) and verifies (`:1071`). It returns
  `(SourceMap, MirModule)`.
* `build_file` (`:1128`) and `build_to` (`:1160`) are near-duplicates: both
  emit the IR, write it to `%TEMP%/nexum_out.ll`, call
  `verify_ir_for` with the target's triple (`:1141`, `:1167`), and then
  `run_clang_with` (`:1142`, `:1168`).
* `lex_file` (`:1081`) prints `<kind>\t\n<text>\n` per token — the format
  the self-hosted lexer must reproduce exactly.
* `parse_file` (`:1105`) prints `nexum_parser::normalize::normalize(&module)`
  — the format the self-hosted parser must reproduce exactly.
* `format_nx` (`:522`) is `nx fmt`: it lexes, parses and then re-prints the
  AST with hand-written `fmt_type` (`:549`), `fmt_expr` (`:587`),
  `fmt_path` (`:663`) and `fmt_stmt` (`:679`) functions. Every `match` in
  them is exhaustive over the parser's enums.
* `explain` (`:947`) is a `match` from the diagnostic code to a one-line
  message; it currently covers T001–T049.
* `current_target` (`:1030`) maps `--target` to a `TargetSpec` through
  `TargetSpec::by_name`, falling back to `TargetSpec::host()`.
* `run_tests` (`:858`) is `nx test`: it runs `nx run` on every `.nx` in a
  directory. It is *not* the Rust integration test suite.

### The lexer — `crates/nexum-lexer/src/lib.rs`

* `TokenKind` (`:7`) has 4 literal kinds (`Ident`, `Int`, `Float`, `Str`),
  17 keywords, 12 punctuation tokens, 18 operator tokens, and 4 layout
  tokens (`Newline`, `Indent`, `Dedent`, `Eof`).
* `TokenKind::name()` (`:68`) returns the stable string for each kind. The
  doc comment says exactly why: *"used by `nx lex` so that the self-hosted
  lexer can be compared against this one token by token"*.
* `Token` (`:126`) is `kind` + `span` + `text`; for a `Str` token the text
  is the *decoded* content (the quotes are not part of it).
* `lex(sf)` (`:137`) is the entry point; it returns `LexerOutput`
  (`:132`) with `tokens` and `diagnostics`.
* The scanner is `Lexer::run` (`:185`). The parts a language feature
  touches: the indentation logic (`:188`–`:231`, emitting `Indent`/`Dedent`
  and the L001 diagnostic at `:222`), the string scanner (`:261`–`:302`,
  L002 at `:289`), the number scanner (`:304`–`:330`, which decides `Int` vs
  `Float` by whether a `.` is followed by a digit), the keyword table
  (`:342`–`:360`), the two-character operator table (`:370`–`:383`) and the
  single-character table (`:390`–`:415`, L003 at `:409`).
* Escape sequences are limited to `\n`, `\t`, `\"`, `\\` (`:274`–`:278`);
  anything else is passed through literally.
* Numbers are `i64`/`f64` only; there is no suffix, no hex, no width.

### The parser — `crates/nexum-parser/src/lib.rs` and `src/normalize.rs`

The AST:

* `TypeExpr` (`:13`) — `Named(String)` or `Array(Box<TypeExpr>, usize)`.
* `BinOp` (`:19`) — 13 operators; `UnOp` (`:36`) — `Neg`, `Not`.
* `Expr` (`:42`) is `{ kind: ExprKind, span: Span }`; `ExprKind` (`:48`) has
  `Int`, `Float`, `Bool`, `Str`, `Var`, `Call`, `Binary`, `Unary`, `Field`,
  `Index`, `Array`.
* `PathSeg` (`:81`) — `Field(String)` or `Index(Expr)`; it is how a path
  assignment target is represented.
* `Param` (`:87`) — name, optional type, and `mutable` (which means
  *by-reference*, not "assignable").
* `Stmt` (`:96`) is `{ kind: StmtKind, span: Span }`; `StmtKind` (`:102`) has
  `Fn`, `Let`, `Assign`, `PathAssign`, `TypeDecl`, `If`, `While`, `For`,
  `Return`, `Expr`, `Import`.
* `ForIter` (`:157`) — `Range(Expr, Expr)` or `Each(Expr)`.
* `Module` (`:165`) is a flat `Vec<Stmt>`.

Every node carries a `Span`, and the parser's own test
(`every_node_carries_a_span`, `:1256`) pins that.

Loading and imports:

* `load_module(sm, root)` (`:196`) returns one flattened `Module` plus
  diagnostics. `load_file` (`:204`) canonicalises the path (`:213`) and
  skips a file it has already seen (`:214`), so a diamond import or an
  import cycle terminates by construction. A missing file is T047
  (`:220`, `:253`).

Parsing:

* `parse(tokens)` (`:174`) is the entry point.
* `parse_stmt` (`:336`) dispatches on the token kind. The `Ident` arm
  (`:347`–`:496`) is the interesting one: it *scans ahead* over
  `.field` / `[index]` segments to a top-level `=` to decide whether the
  statement is a path assignment (`:352`–`:412`) or an expression.
* The expression precedence chain is `parse_expr` (`:872`) → `parse_or`
  (`:875`) → `parse_and` (`:892`) → `parse_eq` (`:909`) → `parse_cmp` (`:931`)
  → `parse_add` (`:955`) → `parse_mul` (`:977`) → `parse_unary` (`:1000`) →
  `parse_postfix` (`:1029`) → `parse_primary` (`:1077`). Each level is a
  loop over its own operator set.
* `parse_type_expr` (`:648`) accepts a name or `[T; N]`.
* Parser diagnostics are P001–P015.

The normalised form — `crates/nexum-parser/src/normalize.rs`:

* The doc comment (`:1`–`:10`) states the contract: a line-oriented tree, two
  spaces of indent per level, *"deliberately mechanical - it prints what the
  parser built, not what the program means - so that it stays a parser-level
  contract"*.
* `binop_name` (`:14`) and `unop_name` (`:32`) map operators to names.
* `type_expr` (`:54`) prints `named <n>` or `array <n>`.
* `expr` (`:66`) prints `int`, `float`, `bool`, `str`, `var`, `call`, `binop`,
  `unop`, `field`, `index`, `array`.
* `stmt` (`:107`) prints `fn`, `let`/`let-mut`, `assign`/`assign-op`,
  `assign-path`/`assign-path-op`, `type`, `if`/`then`/`else`, `while`, `for`
  (+ `range`/`each`), `return`, `expr`, `import`.

### The sema — `crates/nexum-sema/src/lib.rs`

The type representation:

* `Ty` (`:15`): `Int`, `Float`, `Bool`, `Str`, `Unit`, `Record(String)`,
  `Array(Box<Ty>, usize)`, `List(Box<Ty>)`, `Never`, `Unknown`, `Error`.
* `Ty::name` (`:35`) prints a user-facing name; `Ty::is_numeric` (`:50`) is
  `Int | Float`; `Ty::compatible` (`:54`) treats `Unknown`, `Error` and
  `Never` as compatible with everything, compares arrays element-wise *and*
  by length, and lists element-wise.
* **`Ty` is the compiler's single type representation and it is shared**:
  `nexum-mir` imports it (`crates/nexum-mir/src/lib.rs:6`) and `nexum-codegen`
  imports it (`crates/nexum-codegen/src/lib.rs:4`). A new type therefore
  lands in sema *and* is visible in MIR and codegen without a new import —
  but it also means the sema file is contended between the "add a type" agent
  and the "add a builtin" agent.

The HIR:

* `HirModule` (`:74`) — `records: Vec<(String, Vec<(String, Ty)>)>` and
  `functions: Vec<HirFn>`.
* `HirFn` (`:81`) — name, `params: Vec<(String, Ty)>`, `mut_params: Vec<bool>`
  (parallel to `params`), `ret: Ty`, `body: Vec<HirStmt>`.
* `HirStmt` (`:91`) — `Let`, `Assign`, `If`, `While`, `For`, `Return`, `Expr`.
  Note that `HirStmt` has **no** `TypeDecl` and **no** `PathAssign`: records
  live in `HirModule::records`, and a path assignment is desugared into
  nested `FieldSet`/`IndexSet` expressions.
* `HirExpr` (`:124`) — every variant carries its `Ty`; `HirExpr::ty()`
  (`:183`) reads it out.

The analysis:

* `analyze(am)` (`:208`) is a fixed sequence:
  * pass 0a (`:209`–`:225`) collects record *names* first, so a field may
    refer to a record declared later; a duplicate name is T022.
  * pass 0b (`:226`–`:269`) resolves the field *types*; a duplicate field is
    T023 (`:236`), an unknown type is T024 (`:249`), an empty record is
    T027 (`:259`).
  * pass 0c (`:271`–`:312`) collects the function signatures; a duplicate
    function is T014 (`:280`).
  * then a **fixpoint loop** (`:314`–`:403`): the body analysis refines
    `Unknown` parameter types from call sites and `Unknown` return types
    from `return` statements, and the loop re-runs until the signature table
    stops changing (at most 65 iterations). When it converges, the remaining
    `Unknown`s get ABI defaults: an untyped parameter becomes `Int`, an
    untyped return becomes `Int` if the body returns a value and `Unit`
    otherwise (`:386`–`:401`).
  * the HIR is `None` if any error diagnostic was produced (`:418`–`:432`).
* `Scope` (`:466`) is a `HashMap<String, VarInfo>` where `VarInfo` is
  `{ ty, mutable }` (`:460`). `Scope::refine` (`:484`) fixes a binding's type
  in place; `Scope::merge_refined` (`:498`) copies refinements of *outer*
  bindings back after a nested scope is analysed — that is how a list's
  element type, fixed by a `list_push` inside a loop body, becomes visible
  outside it.
* `Ctx` (`:447`) holds the diagnostics, the signature table, the record
  table, the current return type, the current function name, and a
  `synthetic` counter for compiler-generated names. Generated names contain
  `$` (`:454`–`:456`), which a user identifier cannot, so they never
  collide.
* `tty` (`:537`) maps the AST's `TypeExpr` to `Ty`; the primitive names it
  recognises are exactly `"Int"`, `"Float"`, `"Bool"`, `"Str"`, `"Unit"`
  (`:539`–`:544`). **This is where a new primitive type name goes.**
* `ty_check` (`:559`) rejects a zero-length array (T030) and a `Unit`/`Never`
  element type (T036).
* `analyze_block` (`:599`) analyses statements. The for-each desugaring is
  at `:793`–`:862`: `for v in xs` becomes `let $feaN = xs`, `let $feiN = 0`,
  and a `while $feiN < len` whose body re-binds `v` from `$feaN[$feiN]` and
  increments the index. (The array is bound to a temporary *before* the
  loop, which is why mutating the original inside the body does not affect
  the iteration — D008.)
* `analyze_expr` (`:903`) is the builtin dispatch. In order: record
  construction (`:1128`–`:1167`), `panic` (`:1168`–`:1183`, type `Never`),
  `str_char_at` (`:1184`), `slice` (`:1220`), `read_file` (`:1260`),
  `write_file` (`:1286`), `list_new` (`:1316`), `list_len` (`:1332`),
  `list_push`/`list_set` (`:1359`), `list_get` (`:1451`), `concat` (`:1502`),
  `collect` (`:1529`), `heap_used` (`:1545`), `len` (`:1561` — an array
  length is a compile-time constant; a string length is rewritten to a call
  to `nexum_strlen`, `:1577`), `print` (`:1596`), and finally user functions
  (`:1638`–`:1724`). A builtin is *not* a function in the signature table;
  it is a hard-coded arm here, and the code generator lowers it separately.
* `analyze_path_assign` (`:1755`) desugars `a.b[i].c = v` into nested
  `FieldSet`/`IndexSet` expressions rooted at an assignment to `a`. For a
  compound assignment each index expression is bound to a temporary
  (`:1867`–`:1881`) so it is evaluated exactly once.
* `binop_type` (`:1965`) is the operator typing rule: arithmetic requires
  numerics of the same type and yields `Float` if either side is `Float`;
  `Eq`/`Ne` allow `Str` (byte-wise); ordering requires numerics; `And`/`Or`
  require `Bool`.

### The MIR — `crates/nexum-mir/src/lib.rs`

* `Value` (`:10`) and `BlockId` (`:13`) are integer handles.
* `Const` (`:16`) — `Int`, `Float`, `Bool`, `Str`.
* `BinOp` (`:24`) and `UnOp` (`:41`) mirror the parser's.
* `Instr` (`:47`) — `Const`, `BinOp`, `UnOp`, `Call`, `Copy`, `Alloca`,
  `Load`, `Store`, `Record`, `FieldGet`, `FieldSet`, `Panic`, `ArrayLit`,
  `IndexGet`, `IndexSet`.
* `Term` (`:138`) — `Goto`, `Branch`, `Return`, `Unreachable`.
* `BasicBlock` (`:150`) — id, instructions, terminator.
* `MirFn` (`:157`) — name, `params`, `mut_params`, `ret`, `locals`, `blocks`,
  `entry`, and `value_types: HashMap<usize, Ty>` (the type of every SSA
  value, which codegen relies on).
* `MirModule` (`:171`) — `records` and `functions`.
* `lower(hir)` (`:182`) — HIR → MIR. It builds a `fn_mut` table
  (`:184`–`:189`) so that the call lowering knows which parameters are
  by-reference.
* `LowerCtx` (`:202`) — locals, blocks, the current block, a value counter,
  `env: HashMap<String, Value>` (name → slot), `value_types`, the record
  table, `fn_mut`, and a `terminated` flag.
* `lower_fn` (`:241`) — the parameters get values `0..n` (`:258`–`:262`) so
  the code generator can map them to the incoming arguments. A `mut`
  parameter is a borrow: no copy is made and the loads/stores go through the
  caller's storage (`:265`–`:270`); a plain parameter is copied into a
  fresh slot (`:271`–`:284`).
* `lower_stmt` (`:335`) — `Let` (alloca + store), `Assign` (store, and
  `Unreachable` after a `Never`-typed value), `Expr`, `Return`, `If`
  (branch + merge block, with the environment restored across the branches
  so a binding made in a branch does not leak), `While`, `For`.
* `lower_expr` (`:527`) — every HIR expression becomes one or more
  instructions. `panic` is special-cased into `Instr::Panic` (`:750`–`:754`).
  A `mut` argument is passed as the *slot address*, not the loaded value
  (`:736`–`:749`).
* `verify(m)` (`:772`) — today it only checks that every block has a
  terminator (M001). It is the place to add MIR-level invariants.
* The optimizer, at level 1: `const_fold` (`:792`) with `fold_binop`
  (`:867`) and `fold_unop` (`:913`); `dce` (`:926`) with `collect_uses`
  (`:993`); `inline_small` (`:1100`) with `inlinable` (`:1108`) and
  `try_inline_one` (`:1137`). `optimize_level` (`:1383`) runs
  fold → dce → inline → fold → dce.
  **Every one of `collect_uses`, `instr_dest`, `instr_ty`, `remap_inputs` and
  `set_instr_dest` is an exhaustive `match` over `Instr`.** A new
  instruction must be added to all five in the same commit.
* `escape_analysis` (`:1317`) is a small report used by `nx analyze`
  (`main.rs:152`).

### The code generator — `crates/nexum-codegen/src/lib.rs`

* `llvm_ty(t)` (`:8`) is **the type mapping**: `Int` → `i64`, `Float` →
  `double`, `Bool` → `i1`, `Str` → `ptr`, `List` → `ptr`, `Unit` → `void`,
  `Record(n)` → `%n`, `Array(e, n)` → `[n x <e>]`, and everything else
  (`Never`, `Unknown`, `Error`) → `i64`.
* `has_refs(t, records)` (`:23`) is **the collector predicate**: `Str` and
  `List` hold a reference; an array holds one if its element type does; a
  record holds one if any of its fields does.
* `llvm_align` (`:36`), `llvm_size` (`:55`), `record_size` (`:70`) and
  `scan_bytes` (`:85`) are the layout model. `scan_bytes` is 0 for a value
  that cannot hold a reference, 8 for a `Str`/`List`, and the whole size for
  an aggregate that contains references.
* `emit(mir, target)` (`:109`) writes the module header (the triple at
  `:112` and the data layout at `:113`), then the runtime slice
  (`nexum_runtime::runtime_ir(&target.os, &target.arch)`, `:119`), then the
  record type definitions (`%Name = type { ... }`, `:122`–`:125`), then the
  functions, then the string globals.
* `emit_fn(f, ...)` (`:228`) is the whole function body:
  * **The memory plan** (`:281`–`:351`) is a pre-scan that decides the
    prologue. It collects: hoisted `alloca`s (`:304`), a scratch slot per
    array index site (`:305`–`:316`), *managed temporaries* — the spill slot
    for the result of a call whose type holds references (`:317`–`:326`) —
    *element temporaries* for `list_push`/`list_set` (`:327`–`:331`), and
    *aggregate temporaries* for the results of `Record`, `ArrayLit`,
    `FieldSet` and `Copy` when the type holds references (`:336`–`:345`).
  * **The root entries** (`:361`–`:389`) decide what the collector scans:
    every hoisted slot whose type holds references, every managed temporary,
    every element temporary and every aggregate temporary, each with its
    `scan_bytes`.
  * The prologue (`:397`–`:458`) emits the allocas, zeroes every root slot
    (`:418`–`:432`), builds the `%nexum.root_frame` and the
    `[N x %nexum.root_entry]` array, and calls `@nexum_roots_push` (`:456`).
  * The instruction emission is `:459`–`:1172`.
  * `Instr::Call` (`:691`) is where the builtins are lowered: `print`
    (`:716`, to `printf` with a format global), `list_push`/`list_set`
    (`:756`), `list_get` (`:802`), and everything else through the runtime
    symbol table (`:836`–`:847`: `concat` → `nexum_concat`, `collect` →
    `nexum_collect`, `heap_used` → `nexum_heap_used`, `str_char_at` →
    `nexum_str_char_at`, `slice` → `nexum_slice`, `read_file` →
    `nexum_read_file`, `write_file` → `nexum_write_file`, `list_new` →
    `nexum_list_new`, `list_len` → `nexum_list_len`).
  * The terminator (`:1176`–`:1235`) calls `@nexum_roots_pop()` before every
    return (`:1193`–`:1195`) and converts `main`'s value to `i32`
    (`:1202`–`:1220`).
  * Integer division and remainder are fully defined: a zero divisor calls
    `nexum_panic("division by zero")` (`:562`–`:576`), and `MIN / -1` wraps
    to `MIN` while `MIN % -1` is `0` (`:577`–`:612`). No `nsw` is ever
    emitted, because the arithmetic wraps by definition (`:528`–`:530`); the
    test at `backend.rs:86` pins that.

### The runtime — `crates/nexum-runtime/src/lib.rs`, `strings.rs`, `syscalls.rs`

* `runtime_ir(os, arch)` (`:46`) returns the entire runtime as LLVM IR text,
  which the code generator splices into every module. Nothing is linked in
  separately.
* The C-library declarations are at `:49`–`:54` (`printf`, `fputs`, `fputc`,
  `exit`, `malloc`, `free`).
* `strings::string_routines()` (`:58`) adds `nexum_strlen`, `nexum_memcpy`
  and `nexum_strcmp` — ours, on every platform.
* The Windows branch (`:60`–`:74`) uses the UCRT (`__acrt_iob_func`, `fopen`,
  `fclose`, `fseek`, `ftell`, `fread`, `fwrite`).
* The POSIX branch (`:75`–`:97`) uses `syscalls::wrappers` (`:91`) and
  `strings::file_routines(true, ...)` (`:92`–`:96`).
* The root frame types are at `:102`–`:103` (`%nexum.root_frame` is
  `{ ptr, ptr, i64 }`, `%nexum.root_entry` is `{ ptr, i64 }`); the collector
  globals are at `:104`–`:113` (`@nexum_obj_head`, `@nexum_roots_head`,
  `@nexum_live`, `@nexum_since`, `@nexum_threshold` = 65536).
* The collector: `nexum_panic` (`:115`–`:126`), `nexum_oom` (`:128`),
  `nexum_roots_push`/`nexum_roots_pop` (`:133`–`:140`),
  `nexum_alloc_scanned`/`nexum_alloc` (`:142`–`:151`),
  `nexum_gc_maybe` (`:153`), `nexum_mark_ptr` (`:158`),
  `nexum_scan_range` (`:165`), `nexum_collect` (`:171`),
  `nexum_gc_trace` (`:177`), `nexum_sweep` (`:183`).
* The strings: `nexum_concat` (`:189`), `nexum_str_char_at` (`:194`),
  `nexum_slice` (`:199`).
* The lists: `nexum_list_new` (`:218`), `nexum_list_len` (`:220`),
  `nexum_list_push` (`:224`), `nexum_list_get` (`:226`),
  `nexum_list_set` (`:229`). The layout is documented at `:208`–`:216` and
  in `docs/runtime-abi.md` §5.
* `nexum_heap_used` (`:234`).
* `strings.rs`: `string_routines` (`:11`), `file_routines` (`:33`, with a
  POSIX and a C-library variant).
* `syscalls.rs`: `SysOs` (`:25`), `SysArch` (`:32`), `Syscalls` (`:39`) and
  `Syscalls::for_os` (`:58`–`:91`, the per-system numbers: Linux read 0,
  write 1, open 2, close 3, lseek 8, mmap 9, munmap 11, exit 60; Darwin read
  3, write 4, open 5, close 6, lseek 199, mmap 197, munmap 198, exit 1),
  `Layout` (`:94`) and `layout` (`:110`, the register assignment per
  OS/arch), `call_body` (`:168`, the assembly text), `wrappers` (`:237`).

### The target — `crates/nexum-target/src/lib.rs`

* `TargetSpec` (`:4`) is `{ triple, pointer_width, os, arch }`.
* `host()` (`:14`) derives it from `std::env::consts`.
* `by_name(name)` (`:39`) resolves the five named targets; `list()` (`:75`)
  is the canonical list: `windows-x86_64`, `linux-x86_64`, `linux-aarch64`,
  `macos-x86_64`, `macos-aarch64`. Each name also has a triple alias
  (`x86_64-pc-windows-msvc`, …).
* `data_layout_str()` (`:85`) gives the LLVM data layout per (arch, os).
* `is_host()` (`:102`).

### The toolchain — `crates/nexum-toolchain/src/lib.rs`

* `Toolchain` (`:7`) is `{ clang, linker_kind }`; `discover()` (`:13`) finds
  `clang` on the `PATH` and fails with `missing_clang_message` (`:133`) if it
  is not there. The linker is clang's default for now (phase 11g replaces
  it).
* `find_exe` (`:143`) is the `PATH` lookup; `find_vscode` (`:40`) is the
  editor lookup used by `nx update`.
* `BuildOpts` (`:161`) is `{ opt, lto, sysroot, pgo_generate, pgo_use }`.
* `run_clang_with` (`:178`) invokes `clang -O<n> [-flto=thin -fuse-ld=lld]
  [--sysroot=…] [-fprofile-generate|-fprofile-use=…] <ll> -o <out>`.
* `verify_ir_for(clang, ll, target)` (`:221`) compiles the IR to a throwaway
  object file with `-x ir -c`, which runs the LLVM parser, verifier and
  backend. The `--target=` flag matters: a module that contains assembly for
  a foreign instruction set cannot be checked by the host assembler.

### The tests

* `crates/nexum-driver/tests/fixtures.rs` — the front-end gate.
  * `compile_pass_fixtures` (`:14`): every `tests/fixtures/pass/*.nx` must
    lex, parse, type-check, lower to MIR and pass MIR verification with
    **zero** error diagnostics, and must produce MIR.
  * `compile_fail_fixtures` (`:40`): every `tests/fixtures/fail/*.nx` must
    declare at least one `# EXPECT: Txxx` comment (`:48`–`:61`), the
    compiler must produce that code, and **every** diagnostic it produces
    must carry a source span (`:84`–`:92`).
* `crates/nexum-driver/tests/backend.rs` — the back-end gate.
  * `ir_verifies_for_pass_fixtures` (`:44`) and `ir_verifies_for_examples`
    (`:52`): the emitted IR must pass `clang -x ir -c`.
  * `ir_verifies_for_all_targets` (`:60`): the same program must verify for
    **all five** targets, with the target's triple passed to clang.
  * `codegen_emits_expected_fragments` (`:76`): the IR must contain
    `define i32 @main(`, `fadd double`, `mul i64`, and must **not** contain
    `nsw`.
  * `run_outputs_match_fixtures` (`:93`): every `tests/fixtures/run/*.nx` is
    compiled, linked, run, and its stdout must equal the `.out` file byte
    for byte (with `\r\n` normalised); if a `.exit` file exists, the exit
    code must match it.
  * `differential_opt_levels_agree` (`:251`): the same program compiled at
    opt level 0 and 1 must produce the same stdout and the same exit code.
* `crates/nexum-driver/tests/common/mod.rs` — the shared harness:
  `run_frontend` (`:24`), `compile_file` (`:44`, front end + lower +
  `optimize` + `verify`), `emit_ir` (`:64`), `compile_file_opt` (`:69`),
  `nx_files` (`:89`), `fixtures_dir` (`:99`), `examples_dir` (`:106`).

The fixture corpus today:

| Directory | Files | What it proves |
|-----------|-------|----------------|
| `tests/fixtures/pass/` | 6 `.nx` | The feature compiles cleanly end to end (`fixtures.rs:14`) |
| `tests/fixtures/fail/` | 29 `.nx` | The feature is rejected with the right diagnostic code, and the diagnostic has a span (`fixtures.rs:40`) |
| `tests/fixtures/parse/` | 35 `.nx` | The feature parses, and the self-hosted parser agrees with the Rust one (`backend.rs:198`) |
| `tests/fixtures/run/` | 21 `.nx` + 21 `.out` + 4 `.exit` | The feature *behaves*: exact stdout and exit code (`backend.rs:93`) |

The `run/` fixtures cover: arithmetic, arrays, compound assignment, division
by zero, exit codes, `fib`, for-each, GC reclamation, GC stress, index out of
bounds, integer wrapping, file I/O round-trip, lists, list GC, lists of
records under GC, loop sums, nested records, `panic`, particles, records and
strings. The `fail/` fixtures cover: annotation mismatch, arity, array
element mismatch, assignment type mismatch, bad argument type, bad `if`
condition, bad operands, bad `while` condition, `collect` arity, compound
assignment to an immutable, `concat` arity, empty array literal, for-each
over a non-array, `heap_used` arity, immutable assignment, indexing a
non-array, a non-`Int` index, a constant out-of-bounds index, `len` arity,
list arity, list element type unknown, list push mismatch, a `mut` borrow of
a non-mutable, assignment to a parameter, `print` arity, an undefined
function, an undefined variable, an unknown field, and a zero-length array.

### The self-hosting gate

* `examples/selfhost/lexer.nx` is the driver: it reads
  `nexum_selfhost_input.nx` (`:11`), lexes it (`:12`) and prints the token
  stream in the `nx lex` format (`:17`–`:19`).
* `examples/selfhost/lexer_lib.nx` is the lexer itself: the `Token` record
  (`:11`), the keyword table `keyword_kind` (`:37`), the two-character
  operator table `two_char_kind` (`:73`), the single-character table
  `single_char_kind` (`:98`), and `lex_tokens` (`:134`).
* `examples/selfhost/parser.nx` is the parser: the flat `Node` arena
  (`:21`–`:26`), the operator tables (`:79`–`:135`), the expression parsers
  (`:139`–`:351`), the type parser (`:354`), the statement parsers
  (`:377`–`:598`), the normalised-form printer `print_node` (`:638`), and
  `main` (`:766`).
* **The differential test** `selfhosted_lexer_matches_rust_lexer`
  (`backend.rs:142`) compiles the self-hosted lexer once, then for **every
  file in `examples/`** compares its token stream with the Rust lexer's,
  token for token (`:166`–`:193`).
* **The differential test** `selfhosted_parser_matches_rust_parser`
  (`backend.rs:198`) compiles the self-hosted parser once, then for **every
  file in `tests/fixtures/parse/`** compares its normalised form with
  `nexum_parser::normalize::normalize` (`:222`–`:248`).
* The scope note is in `docs/self-hosting.md:40`: the self-hosted parser
  covers functions, `let`, assignment, expression statements, `return`,
  `if`/`else`, `while`, `for`, record declarations, `import` and type
  annotations. **Path assignments (`a.b[i] = v`) are not parsed by it yet**,
  which is why the two examples that use them (`examples/particles.nx`,
  `examples/segments.nx`) are outside the parser differential corpus.

**What a new language feature has to do to keep the self-hosted compiler in
step:**

1. *A new token kind* → add it to `TokenKind` **and** to `TokenKind::name()`
   in `crates/nexum-lexer/src/lib.rs`, and to the matching table in
   `examples/selfhost/lexer_lib.nx` (`keyword_kind` `:37`, `two_char_kind`
   `:73`, `single_char_kind` `:98`). The names must be **byte-identical**,
   because the test compares the printed strings. The `name()` doc comment
   (`:66`–`:67`) says this is the contract.
2. *A new syntactic form* → add it to `crates/nexum-parser/src/normalize.rs`
   (`expr` `:66`, `stmt` `:107` or `type_expr` `:54`) **and** to
   `print_node` in `examples/selfhost/parser.nx` (`:638`), plus the
   corresponding parse function in that file. The line prefixes must match
   byte for byte. A `tests/fixtures/parse/*.nx` fixture that uses the new
   form is what makes the comparison bite.
3. *A change in the meaning of a form, but not its shape* → nothing to do
   in `examples/selfhost/`: the self-hosted lexer and parser only model the
   *shape* of the program (the normalised form is deliberately mechanical —
   `normalize.rs:9`–`:10`). The semantic change lands in sema, MIR, codegen
   and the runtime only.
4. *A change to the token set that an example uses* → the example is
   automatically the input of the lexer differential test, so the self-hosted
   lexer must know the new token before the example lands, or the test fails
   on every example program.

---

## The design

The design this document recommends is not a new architecture; it is the
existing crate split, used as the cutting plane.

**The rule.** One crate, one stage, one owner. A language feature is a
sequence of edits down the pipeline, and the pipeline is already a sequence
of separate files, so the feature can be cut into per-stage slices that do
not overlap — as long as the type representation and the AST are agreed
before the cut, because `nexum_sema::Ty` is imported by both MIR and codegen
and the parser's `ExprKind`/`StmtKind` are matched exhaustively by the
formatter.

**The three contended files.** In practice, three files are wanted by more
than one phase:

* `crates/nexum-sema/src/lib.rs` — `Ty` (`:15`) and the builtin dispatch
  (`:1123`–`:1637`). 11c (the numeric tower) needs `Ty`; 11d (pointers,
  `unsafe`, `volatile`) needs `Ty`; 11e (threads, atomics) needs `Ty`; 11f
  (vectors) needs `Ty`. **One agent must own this file for the whole
  programme**, or the phases must be sequenced.
* `crates/nexum-codegen/src/lib.rs` — `llvm_ty` (`:8`), `has_refs` (`:23`),
  the memory plan (`:281`) and the call lowering (`:691`). The same four
  phases want it.
* `crates/nexum-driver/src/main.rs` — `explain` (`:947`) and `format_nx`
  (`:522`). 11b–11f will each add diagnostic codes, and each will add a
  syntactic form that the formatter must handle.

**The type-first ordering.** Because `Ty` is shared, the natural order
inside the programme is: (1) agree the type vocabulary and the AST shape in
`docs/language-reference/decisions.md` and `grammar.md`; (2) implement the
type in sema, MIR and codegen together; (3) implement the syntax in lexer,
parser and sema together; (4) implement the runtime primitive; (5) extend
the self-hosted compiler; (6) add the fixtures. A phase that skips (5) breaks
`backend.rs:142` or `backend.rs:198` for every other agent.

**The feature-level recipe** is section 4 above; it is written so that the
files can be assigned by index. The key non-obvious points are:

* the `Instr` enum is matched exhaustively in **five** places in the
  optimizer plus one in codegen, so a new MIR instruction is a single
  six-file edit that cannot be split;
* `nx fmt` (`main.rs:522`) is a second implementation of the AST walk and
  must be updated with every new node;
* `nx explain` (`main.rs:947`) is the human-readable table of diagnostic
  codes and must be updated with every new code;
* `examples/selfhost/lexer_lib.nx` and `examples/selfhost/parser.nx` are
  part of the feature, not a follow-up;
* `docs/language-reference/grammar.md` §3/§4/§5 and
  `docs/language-reference/decisions.md` are the specification, and the
  decision entry comes *first*.

---

## Risks

1. **The `Instr` enum is a hidden coupling point.** Adding a MIR instruction
   breaks the build in five optimizer functions and one codegen function.
   This is *good* (the compiler catches it) but it means the MIR+codegen
   slice of a feature cannot be split between two agents, and it means a
   feature that is "MIR only" does not exist. **This would make the
   recommendation wrong** if the phases needed genuinely independent MIR
   work; then the MIR crate would have to be split first, e.g. by moving the
   optimizer into its own crate.

2. **`nexum_sema::Ty` is shared by three crates.** A new type is not a
   sema-local change. If 11c (`f32`) and 11d (pointers) are run in parallel
   and both touch `Ty`, one will lose work. The mitigation is either to
   sequence the phases at the sema level, or to give one agent the ownership
   of `Ty` for the whole programme and have the others submit the type
   additions as a written proposal.

3. **The self-hosted gate is a real coupling point, and it is asymmetric.**
   The lexer differential test runs over `examples/` (every file), while the
   parser differential test runs only over `tests/fixtures/parse/`. A new
   token kind therefore breaks the lexer test for *every* example program,
   while a new statement form breaks the parser test only if a fixture uses
   it. The asymmetry means a feature can land "green" while the self-hosted
   compiler is silently behind, unless a fixture is added. **Recommendation:**
   a feature that adds syntax must add a `tests/fixtures/parse/` fixture that
   uses it, in the same commit.

4. **The examples directory is load-bearing in a way that is not obvious.**
   `examples/*.nx` are IR-verified (`backend.rs:52`) *and* are the input of
   the lexer differential test (`backend.rs:166`). A broken example is a
   broken test, and an example that uses new syntax is a test of the
   self-hosted lexer. So an example is not a demonstration; it is a fixture.

5. **`nx fmt` is a second parser.** It is a hand-written printer over the
   same AST (`main.rs:522`–`:856`). It will not silently produce wrong
   output — the exhaustive matches make it a compile error — but it is
   easy to forget, and it is in the contended driver file.

6. **The `print` builtin still goes through `printf`** (codegen `:716`),
   and the runtime still declares `malloc`, `free`, `printf`, `fputs`,
   `fputc` and `exit` (runtime `:49`–`:54`). A feature that adds a numeric
   type will want a `print` that formats it, which lands in the same
   contended codegen file, and 11a.2 will change the same file. Sequencing
   matters.

7. **`Ty::compatible` is used for everything** (sema `:54`): annotations,
   arguments, assignment, returns, array elements. A new type has to be
   added to `compatible` deliberately, or it will be compatible with
   everything (through the `Unknown`/`Error`/`Never` escape) and the mistake
   will only show up as a missing error diagnostic.

8. **The MIR verifier is nearly empty** (`mir:772`, one check). A feature
   that adds an invariant — a bounds rule, an alignment rule — has no place
   to enforce it today, and nothing will catch a violation. The `verify`
   function is the intended place; the risk is that it is not on anyone's
   list.

---

## Open questions

1. **Should the numeric tower and the pointer work be sequenced at the sema
   level, or should `Ty` be split?** *Recommendation:* sequence them. `Ty` is
   a single enum in a single file, and the phases add variants to it,
   variants to `llvm_ty`, and cases to `has_refs`. Splitting `Ty` into, say,
   a scalar tower and a memory model would decouple them, but it is a
   refactor of the shared representation in the middle of a feature
   programme. Sequencing is cheaper.

2. **Does inline assembly (11b) need a new `Ty`, or is it a statement form
   with a typed operand list?** *Recommendation:* a statement form
   (`StmtKind::Asm`) with an explicit operand list, plus a builtin-ish
   escape hatch, rather than a new `Ty`. A `Ty::Asm` would have to be mapped
   in `llvm_ty` and handled by `has_refs` for no benefit. The codegen
   lowering is then an ordinary `Instr` (or a direct emission from the
   `StmtKind`, which the MIR would have to carry). The open part is whether
   the asm block is an `Instr` in the MIR (so the optimizer sees it) or a
   separate MIR node.

3. **Should the assembly text be validated at compile time?** The runtime's
   assembly is generated by `syscalls::call_body` (`syscalls.rs:168`) and is
   checked by `clang -x ir -c` at verification time. For user-written
   assembly, the same gate would catch a bad constraint — but only after the
   whole module has been emitted. *Recommendation:* keep the LLVM IR verifier
   as the gate (it is already there) and add a MIR-level check for the
   things LLVM cannot catch, such as a register name that is not valid for
   the target.

4. **Should the `Instr` optimizer matches be made non-exhaustive so that
   adding an instruction is cheaper?** They are exhaustive on purpose — the
   compiler catches the missing case. *Recommendation:* keep them exhaustive.
   The cost is one commit touching six places; the benefit is that a new
   instruction cannot be silently mishandled by the optimizer.

5. **Where does the "unsafe" surface of 11d live?** It needs both a type
   (a pointer type in `Ty`) and a permission concept (a region of code that
   may do what the type system otherwise forbids). The type is easy to place;
   the permission is not — it has no home in the current design, where
   `analyze` is a whole-program analysis with no notion of a region.
   *Recommendation:* model `unsafe` as a *marker on a function or a block*
   that the sema checks, rather than as a type-system feature. The open
   question for the orchestrator is whether `unsafe` is a property of a
   function (`fn f() -> Ptr` with an `unsafe` marker) or of a statement
   block.

6. **Should the concurrency primitives of 11e be builtins, or a library?**
   The builtin route is the current one (every builtin is a hard-coded arm
   in `sema:1123`–`:1637` plus a runtime entry point). A library route
   needs a standard library, which `docs/standard-library.md:50` still lists
   as planned and which needs the module system to grow.
   *Recommendation:* the primitives (`atomic_*`, thread spawn/join) are
   builtins; the synchronisation structures (channels, queues) are a
   library. But that decision has to be made before the sema builtin
   dispatch is extended, because it decides whether the 11e agent owns
   `sema` or the standard-library directory.

7. **Should the self-hosted parser be brought up to parity before 11b, or
   in step with it?** It is behind already (no path assignments). *Recommendation:*
   bring the *lexer* and *parser* to parity with the Rust ones at the start
   of the programme, in one slice, so that the differential gate is
   meaningful for every subsequent feature. That is a separate, well-bounded
   task: extend `examples/selfhost/parser.nx` with the path-assignment form
   and add a `tests/fixtures/parse/` fixture that uses it.

8. **Who owns the fixture directories while five phases run in parallel?**
   All of them want `tests/fixtures/pass/`, `fail/`, `parse/` and `run/`.
   *Recommendation:* the fixture files are per-feature and are named after
   the feature, so they do not conflict — but the *harness* files
   (`tests/backend.rs`, `tests/fixtures.rs`, `tests/common/mod.rs`) are
   shared, and `backend.rs:251` (`differential_opt_levels_agree`) has a
   hard-coded list of programs that a phase would want to extend.
   *Recommendation:* that list should be replaced by a directory scan
   (it is the only place in the test suite that is not a scan), so that a
   new fixture is covered automatically.
