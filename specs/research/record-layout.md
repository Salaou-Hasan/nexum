# Record layout and the Nexum ABI

**Question.** How does Nexum lay out a `record` in memory, what is the calling
convention between Nexum functions, and how does the compiler know the size
and alignment of a record type it has never seen at code generation time?
This matters for phase 11d (the record types wave), and the runtime's own
records (the allocator's blocks, the list type) are built the same way.

**Summary.** A record is a value type: an LLVM struct whose fields are
emitted in declaration order with no implicit padding in the type string,
but whose field offsets, size and alignment are computed by the compiler in
`crates/nexum-codegen/src/lib.rs:70` (`record_size`) and used for roots,
scanning and arithmetic. The compiler already computes a natural layout
(alignment = max field alignment, size rounded up to it, fields placed at
the next multiple of their alignment); phase 11d's job is to make that
computation the *promise* rather than an accident, by emitting the padding
explicitly so the layout does not depend on LLVM's data layout. The ABI
between two Nexum functions is the LLVM C ABI: records and arrays are
passed and returned by value, `Str`/`List`/pointers are eight-byte pointers,
mut parameters are passed by pointer, and `main` returns `i32`. The
compiler does not need to "see" a record type at code generation time to
know its size, because the record table (`mir.records`) is part of the MIR
module and is passed into `emit` alongside the functions; `llvm_size`/`llvm_align`
look the record up by name in that table.

## Recommendation

1. **Keep the natural layout as the default, computed by the compiler.**
   Field offsets follow declaration order; each field is placed at the next
   multiple of its alignment; the record's alignment is the maximum of its
   fields'; its size is the end of the last field rounded up to that
   alignment. This is already what `record_size` does
   (`crates/nexum-codegen/src/lib.rs:70-81`), and it matches the table in
   `memory-layout.md` §4. *Reason:* changing the default would break every
   existing record and the self-hosted parser's `Node` record, for no gain;
   the work is to *pin* the existing arithmetic, not to replace it.
2. **Emit the padding explicitly.** Today the record is emitted as
   `%Name = type { i64, i64, i1 }` (`crates/nexum-codegen/src/lib.rs:133-135`)
   and LLVM lays it out. For the layout to be a promise, the compiler must
   emit `[N x i8]` padding fields between the real ones, so the layout is
   identical on every target and does not depend on LLVM's data layout string.
   *Reason:* memory-layout.md §4 and the ABI both need offsets that are the
   compiler's, not the backend's.
3. **Make `mir.records` the single source of truth.** It is already a
   `Vec<(String, Vec<(String, Ty)>)>` (`crates/nexum-mir/src/lib.rs:186`)
   and is cloned into the module at lowering time (`:208`). Codegen receives
   it through `emit(mir, target)` and looks records up by name in
   `llvm_size`/`llvm_align`/`has_refs`. *Reason:* this is how the compiler
   knows the size and alignment of a record it has never seen before
   lowering - the type information travels with the module, it is not
   recovered at code generation time.
4. **Keep records as value types in the ABI.** They are passed and returned by
   value as LLVM structs; `Str`, `List` and `Ptr` are eight-byte pointers;
   mut parameters are passed by pointer to the caller's storage; `main`
   returns `i32`. *Reason:* this is what the code generator already emits
   (`crates/nexum-codegen/src/lib.rs:229-244`), and it is what the runtime's
   root-frame and collector code assume.
5. **Do not move records onto the heap.** Records and arrays are value types
   and never allocate (`docs/runtime-abi.md` §8); only `Str` and `List` are
   managed. *Reason:* the collector's scan sizes and the root-registration
   logic (`has_refs`/`scan_bytes`) are built on that distinction.

## What the code looks like today

The facts the recommendation depends on.

### Record type definitions

* `crates/nexum-parser/src/lib.rs:696` - `StmtKind::TypeDecl { name, fields }`:
  a record declaration is a type declaration with a name and a list of
  `(field name, field type)` pairs. The self-hosted parser mirrors this at
  `examples/selfhost/parser.nx:28` (`type Node:` with `kind`, `text`, `a`,
  `b`, `c` fields) and `examples/selfhost/parser.nx:547` (record
  declaration parsing).
* `crates/nexum-sema/src/lib.rs:76` - `records: Vec<(String, Vec<(String, Ty)>)>`
  in the HIR; `:257` builds a `HashMap<String, Vec<(String, Ty)>>` during
  analysis; `:477` orders it into `ordered_records`.
* `crates/nexum-mir/src/lib.rs:185-186` - `MirModule.records:
  Vec<(String, Vec<(String, Ty)>)>`, "Record definitions: name -> fields".
* `crates/nexum-mir/src/lib.rs:208` - `records: hir.records.clone()` at
  lowering time, so the record table travels with the module.

### Lowering to MIR

* `crates/nexum-mir/src/lib.rs:104-108` - `Instr::Record { dest, name,
  fields: Vec<Value> }`.
* `crates/nexum-mir/src/lib.rs:649-658` - `HE::Record { name, args, ty }`
  lowers by lowering each argument expression and emitting `Instr::Record`.
* `crates/nexum-mir/src/lib.rs:1083` - `Instr::Record { fields, .. }` is
  handled in the MIR passes (DCE/liveness); `:1125` and `:1385` list it as
  a def that may be dropped.
* `crates/nexum-mir/src/lib.rs:532-538` - `field_index` resolves a field name
  to its positional index by scanning `ctx.records`; field access is by
  index, not by name, in the MIR.
* `crates/nexum-mir/src/lib.rs:660-695` - `HE::Field` lowers to
  `Instr::FieldGet { base, index, ty }` and `HE::FieldSet` to
  `Instr::FieldSet { base, index, value, ty }`.

### Code generation

* `crates/nexum-codegen/src/lib.rs:8` - `llvm_ty`: `Ty::Record(n)` maps to
  `%n` (the LLVM struct type name); records are first-class LLVM values.
* `crates/nexum-codegen/src/lib.rs:23` - `has_refs`: a record is a reference
  when any of its fields is; `:30` scans the record's fields.
* `crates/nexum-codegen/src/lib.rs:36` - `llvm_align`: a record's alignment
  is the max of its fields', defaulting to 1.
* `crates/nexum-codegen/src/lib.rs:55` - `llvm_size`: a record's size is
  `record_size(fields, records)`.
* `crates/nexum-codegen/src/lib.rs:70-81` - `record_size`: the natural layout
  (round each field up to its alignment, then round the total up to the
  record's alignment).
* `crates/nexum-codegen/src/lib.rs:85-99` - `scan_bytes`: a record scans
  `record_size` bytes when it contains a reference.
* `crates/nexum-codegen/src/lib.rs:133-135` - emission:
  `%Name = type { i64, i64, i1 }` (LLVM lays it out); no explicit padding.
* `crates/nexum-codegen/src/lib.rs:987-1000` - `Instr::Record` lowers to a
  chain of `insertvalue` into an `undef` struct value.
* `crates/nexum-codegen/src/lib.rs:1016-1030` - `Instr::FieldGet` lowers to
  `extractvalue`.
* `crates/nexum-codegen/src/lib.rs:229-244` - function signature emission:
  non-mut params are `llvm_ty(t) %arg_name` (records by value), mut params
  are `ptr %arg_name`; return type is `llvm_ty(&f.ret)` except `main`, which
  returns `i32`.

### The collector

* `crates/nexum-runtime/src/lib.rs:160` - `nexum_alloc_scanned(payload, scan_bytes)`:
  32-byte header (`+0 size, +8 mark, +16 next, +24 scan_bytes`), payload at
  `+32`, 16-byte aligned. Records that contain references are allocated with
  `scan_bytes = record_size`; records without references are not heap objects
  at all (value types).
* `crates/nexum-codegen/src/lib.rs:367-379` - root registration: a local
  whose type can hold a reference is registered with `scan_bytes` bytes; a
  record local registers the whole size when `has_refs` is true.
* `crates/nexum-runtime/src/lib.rs:184` - `nexum_scan_range`: scans
  `bytes / 8` words from a base, treating each word as a candidate pointer.
* `docs/runtime-abi.md` §2 and §5 - object block layout and list layout; the
  list element buffer's `scan_bytes` is computed from the element type.

### The self-hosted representation

* `examples/selfhost/parser.nx:21-26` - `Node` record with `kind`, `text`,
  `a`, `b`, `c` fields; the whole compiler's AST is an arena of these.
* `examples/selfhost/parser.nx:279-307` - variable-length child lists stored
  in a side table, exactly the pattern the MIR uses for `Instr::Record`'s
  field payloads.
* `self-hosting-plan.md` - records cannot hold a `List` today (F1), so the
  self-hosted compiler uses a `List` of `Entry` records as a linear map.

## The design

### Record layout rules

The layout is computed by the compiler and is Nexum's, not the backend's
(memory-layout.md §4). Concretely:

* Fields are placed in **declaration order**.
* A field is placed at the next offset that is a multiple of its
  **alignment**.
* A type's alignment and size:

  | Type | Alignment | Size |
  | ---- | --------- | ---- |
  | `Bool` | 1 | 1 |
  | `Int` | 8 | 8 |
  | `Float` | 8 | 8 |
  | `Str` | 8 | 8 |
  | `Ptr` | 8 | 8 |
  | `Unit` | 1 | 0 |
  | `[T; N]` | alignment of `T` | `N * size_of(T)` |
  | a record | max of its fields' | total, rounded up to the alignment |

* A record's size is the end of its last field, rounded up to the record's
  alignment.
* A record's alignment is at least 1.

`record_size` already implements this (`crates/nexum-codegen/src/lib.rs:70-81`).
The compiler must emit the padding explicitly as `[N x i8]` fields, so the
layout does not depend on LLVM's data layout string.

### Attributes

* `layout c` - the layout must equal the C layout for the same field types;
  the compiler checks independently and rejects otherwise (T057).
* `layout packed` - alignment 1, no padding.
* `layout align N` - alignment `N` (power of two), size rounded up to `N`.

`layout c` and either of the other two together is an error (T058).

### How the compiler knows size and alignment at code generation time

`emit(mir, target)` receives the whole `MirModule`, including
`mir.records` (`crates/nexum-mir/src/lib.rs:186`). `llvm_size`, `llvm_align`,
`has_refs` and `scan_bytes` all take `records: &[(String, Vec<(String, Ty)>)`
and look the record up by name. There is no separate type database consulted
at code generation: the record table is part of the module, produced during
lowering (`crates/nexum-mir/src/lib.rs:208`). A record type that has never
been seen before lowering time is simply present in that table by the time
`emit` runs; codegen never needs to rediscover it.

### Calling convention

* `Str`, `List`, `Ptr` (and `Arena`) are eight-byte pointers, passed in
  registers/stack slots per the target's C ABI.
* `Int`/`Float` are 64-bit scalars; `Bool` is `i1`; `Unit` is `void`.
* Records and arrays are value types and are passed and returned **by value**
  as LLVM structs/arrays.
* `mut` parameters are passed as `ptr` to the caller's storage
  (`crates/nexum-codegen/src/lib.rs:234-240`).
* `main` returns `i32` (`:229-232`); other functions return `llvm_ty(&f.ret)`.
* The root frame (`prev`, `entries`, `count`) and root entries (`addr`,
  `scan_bytes`) are compiler-generated on the stack per
  `docs/runtime-abi.md` §3, and are popped before every return.

### Record creation and field access in IR

* `Instr::Record { dest, name, fields }` lowers to a chain of `insertvalue`
  into an `undef` `%name` value (`crates/nexum-codegen/src/lib.rs:987-1000`).
* `Instr::FieldGet { base, index, .. }` lowers to `extractvalue`
  (`:1016-1030`).
* Field resolution happens in the MIR by positional index
  (`crates/nexum-mir/src/lib.rs:532-538`), so field names do not survive
  into code generation.

## Risks

* **LLVM's layout may disagree with the compiler's arithmetic today.** Because
  the record is emitted as `%Name = type { ... }` without explicit padding
  (`crates/nexum-codegen/src/lib.rs:133-135`), the offsets LLVM computes can
  differ from `record_size`'s in edge cases (e.g. a `Bool` followed by an
  `Int`), and `extractvalue`/`insertvalue` indexing assumes the compiler's
  numbering. *Mitigation:* emit explicit `[N x i8]` padding fields, as
  recommended.
* **Records containing references must be roots.** If a record local is not
  registered (because `has_refs` returns false incorrectly), the collector
  can free a string it points at. *Mitigation:* `has_refs` already recurses
  into records (`crates/nexum-codegen/src/lib.rs:27-31`); keep that.
* **The self-hosted compiler depends on the layout being stable.** Its
  `Node` record (`examples/selfhost/parser.nx:21-26`) and the bootstrap gate
  rely on declaration-order fields with the natural alignment. Changing the
  default layout would break the self-hosted front end.
* **`layout c` is mostly a promise today.** For the current type set the
  natural layout already equals the C layout, so the check only bites once
  `packed` or `align N` or non-C types exist. *Mitigation:* implement the
  check now so it is ready.

## Open questions

1. **Should the record's padding be emitted as anonymous `[N x i8]` fields or
   as named padding fields?** *Recommendation:* anonymous fields in the LLVM
   struct type, since field names do not survive into code generation anyway
   (access is by positional index in the MIR).
2. **Should records ever be passed by reference (pointer) instead of by
   value?** *Recommendation:* no - keep by-value; mutability of a record
   argument is expressed by the existing `mut` parameter mechanism (passed as
   `ptr`), which already covers the case where the caller's storage must be
   mutated.
3. **Where does explicit padding syntax for protocol structs land?** The
   numeric tower (phase 11c) is needed for one-byte/four-byte integer types
   that can be used as padding fields. *Recommendation:* defer explicit
   padding fields to 11c, but ship the compiler-computed layout now.
4. **Should `size_of`/`align_of` be compile-time constants or runtime
   builtins?** *Recommendation:* compile-time constants for concrete types,
   as memory-layout.md §4 specifies; runtime builtins only for generic/unknown
   types, which do not exist yet.

## Changes needed elsewhere

* `docs/runtime-abi.md` §1 should state the explicit-padding promise once
  phase 11d lands, and §8 should note that records remain value types.
* `docs/language-reference/decisions.md` needs a numbered entry (D017 or
  later) recording the layout rules and the `layout c`/`packed`/`align N`
  attributes, per AGENT-BRIEF.md §"The house rules".
* `docs/language-reference/grammar.md` needs the `layout` attribute syntax and
  the `type` declaration form, which already exists for fields but not for
  attributes.