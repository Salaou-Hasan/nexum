# The model framework, and what each phase has to give it

**Question.** What does the machine learning framework that phase 11i delivers
actually look like inside Nexum - the tensor, the autodiff, the layers, the
optimisers, the data pipeline, the training loop - and what must phases 11a
to 11h deliver for it to exist at all? The framework is the thing the whole
native roadmap exists for, so the answer is a contract: this document states
the framework's shape and the price of each phase.

**Summary.** The recommendation is a staged plan of eight stages (0 to 7), and
the first stage is
buildable today, in f64, once one compiler defect is fixed: the code generator
emits `icmp` for every float comparison, so `if x > 0.0` does not compile at
all (`crates/nexum-codegen/src/lib.rs:651-662`, with
`crates/nexum-sema/src/lib.rs:2017-2043` typing every comparison as `Bool`).
Without that fix there is no ReLU, no max, no argmax, no early stopping and no
NaN check, so no model can be trained. Given the fix, the framework is
expressible in the language as it stands: a tensor is a flat `List` of `Float`,
a model is a `List` of `List` of `Float`, and the autodiff is a tape of tagged
nodes with one backward function and a static dispatch on the tag - the only
option when there are no closures and no function pointers. What the language
cannot express is a *self-describing* tensor: a record cannot hold a `List`
(`T024`), and a list's element type can only be fixed by a push into a plain
variable (`T049`), so the shape must travel beside the data as `Int`
parameters until the type grammar learns `List[T]`. The dependency table in
section 6.8 says what each phase owes; two items are not on any phase's list
(the float comparison defect and `List[T]`) and the orchestrator has to place
them.

## Recommendation

1. **Fix the float comparison defect before anything else.** It is not a
   phase, it is a bug, and it is a total blocker: every one of `==`, `!=`,
   `<`, `>`, `<=`, `>=` on `Float` emits `icmp` on doubles and the program
   fails LLVM IR verification. The fix is in `crates/nexum-codegen/src/lib.rs`:
   branch on the *operand* type (`oty_str`, or the operand's `Ty`), not on the
   comparison's result type (`ty`, which is always `Bool`), and emit `fcmp
   oeq/one/olt/ogt/ole/oge`. Section "Changes needed elsewhere" has the
   detail.

2. **A tensor is a flat `List` of `Float`; the shape travels beside it, not
   inside it.** A record cannot hold a list (`T024`, `crates/nexum-sema/src/lib.rs:244-256`),
   so `type Tensor: data: List` does not compile today. The framework
   therefore passes `(buffer, dims...)` explicitly: `fn t_matmul(a: List, m: Int,
   k: Int, b: List, n: Int) -> List`. This is not a workaround to be proud of;
   it is the reason the framework's public shape is awkward, and it is a
   standing argument for the type-grammar change in item 3.

3. **Ask for `List[T]` in the type grammar.** One line in `tty`
   (`crates/nexum-sema/src/lib.rs:537-555`) plus the T049 relaxation at
   `crates/nexum-sema/src/lib.rs:1399-1419` (a list *field's* element type
   must be declarable, not only refinable) would let `type Tensor: data:
   List[Float], shape: List[Int], strides: List[Int]` exist, and would let the
   model be one value instead of five parallel lists. Until then the framework
   is written against the parallel-list convention, which is ugly but works.

4. **The autodiff is a tape of tagged nodes, with one backward function.** No
   closures and no function pointers exist, so a backward *function* cannot be
   stored on a node. The node stores a **tag** (`kind: Int`) and the backward
   pass dispatches on it with a static `if`/`else` chain into ordinary named
   functions. The dispatch belongs in the framework library, not in the
   runtime: the runtime is the memory and syscall layer and must not know the
   framework's node layout. The tape is an arena of node records (records
   cannot contain themselves, `docs/language-reference/decisions.md`, D009)
   plus two parallel lists - the values and the gradients - each a list of
   `List[Float]`.

5. **A model is data, not code.** The parameters are `List[List[Float]]` (one
   entry per parameter tensor), the topology is `List[Int]` (the widths and the
   activation codes). That is what makes a model trainable, savable and
   inspectable, and it is expressible today because `List` of `List` works
   (`t4.nx` in the appendix).

6. **Write the mathematics in Nexum before 11c lands, then delete it.** A
   hand-written `exp`, `log` and `tanh` in Nexum source is possible today (the
   language has `+ - * /` and IEEE 754 semantics), so the framework can be
   *proven* before the numeric tower exists. 11c then replaces them with the
   fast builtins. This is the difference between "the framework is blocked on
   11c" and "the framework is blocked on a compiler bug".

7. **The staged plan** is in section 6.9. Stage 1 (the tensor core and the
   training loop for a two-layer MLP on a CSV, in f64) is specified down to
   the function signatures in section 6.10 and can be started by an
   implementer today.

8. **The honest scope is a multilayer perceptron on a tabular dataset.**
   Images need a fourth dimension, strided views, convolution, binary file
   input and the GPU path; section 6.11 says which of those need which phase.

## What the code looks like today

All facts below were read at commit `316f6c6` and, where marked, verified by
running the compiler in this worktree. The probe programs and their outputs
are in the appendix.

### The type system

* `Ty` has exactly eight variants: `Int`, `Float`, `Bool`, `Str`, `Unit`,
  `Record(String)`, `Array(Box<Ty>, usize)`, `List(Box<Ty>)`, plus `Never`,
  `Unknown`, `Error` (`crates/nexum-sema/src/lib.rs:15-31`). There is no
  function type, no closure, no generic parameter, no enum and no type alias.
  A grep for `closure`, `fn_ptr` and `function pointer` over `crates/*/src`
  returns nothing.
* `Float` is `f64` (`docs/runtime-abi.md:82`; `Ty::Float` maps to `double`).
  There is no `f32`.
* The builtins are the whole standard library today
  (`docs/standard-library.md:9-35`): `print`, `panic`, `len`, `concat`,
  `slice`, `str_char_at`, `collect`, `heap_used`, `read_file`, `write_file`,
  and the five `list_*` builtins. **There is no `sqrt`, no `exp`, no `log`,
  no `tanh`, no `erf`, no `pow`, no `abs`, no trigonometric function.**
* There is no conversion between `Int` and `Float` in either direction, and
  no implicit one (`docs/language-reference/decisions.md`, D004). `Int(f)` is
  an undefined function (`T006`).
* `and` and `or` do not short-circuit: the code generator emits LLVM `and` and
  `or` on `i1` (`crates/nexum-codegen/src/lib.rs:663-664`).
* There are no bitwise operators and no shifts: the operator list is
  `docs/language-reference/grammar.md:40`, and there is no `&`, `|`, `^`,
  `<<` or `>>`. Bit-level work has to be done with division and remainder.
* There is no `break` and no `continue` (the keyword list is
  `docs/language-reference/grammar.md:19`). Early exit is done with a flag.
* There are no globals: a top-level `let` is not visible inside a function
  (`T004`, verified).
* `import "path.nx"` works and puts the imported declarations in one shared
  namespace (`docs/language-reference/decisions.md`, D002; verified). A library
  file must not define `main` - two `main`s are `T014`.

### The three structural limits that shape the whole design

These are the facts that make the framework look the way it does.

1. **A record cannot hold a list.** `tty` (`crates/nexum-sema/src/lib.rs:537-555`)
   resolves a field's type expression to `Int`, `Float`, `Bool`, `Str`, `Unit`,
   a record name or an array; `List` is none of these, so it resolves to
   `Unknown` and the T024 check at `crates/nexum-sema/src/lib.rs:244-256`
   rejects it. Verified:

   ```
   error[T024]: unknown type `List` for field `data`
   ```

   So `type Tensor: data: List, shape: List` - the natural shape of a tensor -
   is not expressible. Neither is `type Model: weights: List`.

2. **A list's element type can only be fixed by a push into a plain
   variable.** At `crates/nexum-sema/src/lib.rs:1399-1419`, when the element
   type is `Unknown`, the compiler tries to refine *the binding* named by the
   first argument; if the argument is a field access the refinement fails and
   the push is `T049`. And `list_get` on a list whose element type is still
   unknown is `T049` too (`crates/nexum-sema/src/lib.rs:1476-1485`). So even
   if `List` were allowed as a field type, a list *inside* a record would be
   neither readable nor writable.

3. **Records cannot contain themselves** (D009), so the autodiff tape cannot
   be a tree of records; it has to be an arena of nodes with `Int` indices.

The good news: `List` of `List` works (`t4.nx`), a list element is copied on
read but a list *handle* is shared (`t10.nx`), and `mut` parameters borrow the
caller's binding so a list can be grown in place (`t9.nx`).

### The runtime's list and string support

* The list object is a heap object whose payload is `[element buffer (ptr),
  length, capacity, element size, element scan size]`, and the element buffer
  is a *separate* heap object so growth never moves the list handle
  (`crates/nexum-runtime/src/lib.rs:208-231`, `docs/runtime-abi.md:190-203`).
  Growth doubles the capacity.
* `list_get` and `list_set` are bounds-checked at run time and panic with
  `index out of bounds`.
* The element type is fixed by the first push (D011), which is what makes the
  runtime monomorphic: it stores an element *size* and a *scan size*, not a
  type.
* A `Float` element has a scan size of zero, so a tensor buffer is never
  scanned by the collector - the roadmap calls this out
  (`docs/native-roadmap.md:117`), and it is the reason a tensor in a `List` is
  cheap to keep alive.
* `read_file` reads a whole file into a NUL-terminated `Str`
  (`crates/nexum-runtime/src/strings.rs:39-42`); it panics with `file not
  found` if the file cannot be opened. **Binary input is not supported**: a
  NUL byte truncates the value (`docs/runtime-abi.md:232-233`). A CSV is text,
  so it is fine; a PNG is not.
* `s[i]` on a string allocates: `nexum_str_char_at` calls `nexum_alloc(1)` for
  every byte (`crates/nexum-runtime/src/lib.rs:194-197`). A byte-at-a-time CSV
  scanner that used `s[i]` would allocate one heap string per byte of the file
  and trigger a collection every 64 KiB of file. The runtime needs a
  `str_byte_at(s, i) -> Int` that does not allocate.
* `print` takes exactly one argument and rejects records (`T013`, `T028`).
  A float prints with `%g` (`crates/nexum-codegen/src/lib.rs:725`), i.e. six
  significant digits, and `5.0` prints as `5`. A loss curve wants more.
* The collector is non-moving and single-threaded
  (`docs/language-reference/decisions-gc.md`), and `nexum_alloc` is a plain
  `declare ptr @malloc(i64)` (`crates/nexum-runtime/src/lib.rs:52`) with no
  `noalias` attribute - see section 6.7.

### The compiler defect that blocks the framework

Every float comparison is broken:

```
$ nx run t15.nx          # if 1.0 == 1.0:
C:\nexum_out.ll:623:17: error: icmp requires integer operands
  623 |   %v2 = icmp eq double %v0, %v1

$ nx run t14.nx          # if v > best:
C:\nexum_out.ll:640:19: error: icmp requires integer operands
  640 |   %v15 = icmp sgt double %v13, %v14
```

The cause: `binop_type` returns `Ty::Bool` for all six comparison operators
(`crates/nexum-sema/src/lib.rs:2017-2043`), and the code generator tests
`*ty == Ty::Float` when choosing between `icmp` and `fcmp`
(`crates/nexum-codegen/src/lib.rs:615-662`). `ty` is the comparison's *result*
type, so the `fcmp` branch at line 624 is dead code, and the ordering
operators have no float branch at all.

The framework cannot be written without this fix. ReLU is `max(0, x)`; a
reduction needs `max`; softmax needs the row maximum; cross-entropy needs
`argmax`; early stopping needs `loss < best`; and the NaN check is the
classic `x != x`. Every one of those is a float comparison.

### Measured behaviour (this machine, `--release` build)

* 10^7 dependent float additions: ~15 ms of work (~1.5 ns each). A scalar
  f64 loop is latency-bound on the dependency chain, not on the arithmetic.
* 10^6 multiply-adds, each with a `list_get` out of a `List[Float]`: ~12-18
  ms, i.e. roughly 60-80 M MAC/s. The list access is not the bottleneck; the
  dependent chain is.
* The compiler's own IR passes run at level 0 by default (`--opt-level`, "0 =
  none"); the clang optimisation level is 2 by default and 3 with `--release`,
  which also enables thin LTO.

These numbers are the baseline the tensor kernels have to beat. They also say
that the *shape* of the kernel matters more than the arithmetic: a matmul with
four independent accumulators would already be several times faster, which is
what 11f's vectors are for.

## The design

### 6.1 The tensor

**Representation.** A tensor is a flat, contiguous, row-major buffer of
`Float` held in a `List[Float]`. The element type is fixed by the first push
and the runtime stores the element size, so a buffer of `Float` is a
contiguous run of doubles with no per-element overhead and no collector
scanning.

**Shape and strides.** A tensor also has a shape and a set of strides. Today
the shape cannot be stored with the data (section "the three structural
limits"), so it travels with the call:

* a rank-2 tensor is `(buffer, rows, cols)`;
* the strides are implied by row-major layout: `stride[0] = cols`,
  `stride[1] = 1`;
* a general rank-n tensor is `(buffer, dims: List[Int])`, and the strides are
  derived: `stride[r] = prod(dims[r+1..])`.

The target design (section 6.9, stage 5) is the strided one, where a step
works in one flat buffer and a tensor is `(buffer, offset, dims, strides)`, and
a *view* - a transpose, a slice, a broadcast - is a different
offset-and-stride description of the same memory.
The target kernel signature is therefore

```
fn t_matmul(a: View, b: View) -> View
```

where `View` is a record of `Int`s (`off`, `d0..d3`, `s0..s3`) - which *is*
expressible today, because a record of integers needs no list. The buffer the
view points into still travels beside it, for the same reason the shape does:
a record cannot hold it. The flat form
`fn t_matmul(a: List, m: Int, k: Int, b: List, n: Int) -> List` is the
stage-1 form, and the two differ only in addressing.

**The operations.** In the stage-1 (flat) form:

| Operation | Signature | Notes |
| --------- | --------- | ----- |
| create | `fn t_zeros(n: Int) -> List` | element type fixed by the first push |
| create | `fn t_fill(n: Int, v: Float) -> List` | |
| copy | `fn t_copy(src: List, off: Int, len: Int) -> List` | a contiguous run |
| element-wise | `fn t_add(a: List, b: List) -> List` | same length |
| element-wise | `fn t_mul(a: List, b: List) -> List` | Hadamard |
| element-wise | `fn t_scale(a: List, s: Float) -> List` | |
| broadcast | `fn t_add_vec(a: List, m: Int, n: Int, v: List) -> List` | a length-`n` vector added to every one of the `m` rows |
| broadcast | `fn t_add_scalar(a: List, s: Float) -> List` | |
| reshape | `fn t_reshape(a: List, rows: Int, cols: Int) -> List` | a copy today; a view once strides exist |
| slice | `fn t_slice_rows(a: List, rows: Int, cols: Int, start: Int, count: Int) -> List` | |
| reduction | `fn t_sum(a: List) -> Float` | |
| reduction | `fn t_mean(a: List) -> Float` | |
| reduction | `fn t_max(a: List) -> Float` | needs a float comparison |
| reduction | `fn t_argmax(a: List) -> Int` | needs a float comparison |
| reduction | `fn t_sum_rows(a: List, m: Int, n: Int) -> List` | per-row sums, for the bias gradient |
| reduction | `fn t_max_rows(a: List, m: Int, n: Int) -> List` | per-row maxima, for the softmax |
| matmul | `fn t_matmul(a: List, m: Int, k: Int, b: List, n: Int) -> List` | `a` is m x k, `b` is k x n, row-major |
| transpose | `fn t_transpose(a: List, m: Int, n: Int) -> List` | a copy today; a view (swapped strides) once strides exist |
| gather | `fn t_gather_rows(table: List, rows: Int, cols: Int, ids: List) -> List` | the embedding lookup |
| scatter | `fn t_scatter_add(table: List, rows: Int, cols: Int, ids: List, grad: List) -> List` | the embedding gradient; it **accumulates**, because a token can appear twice in a batch |

**Broadcasting.** Numpy-style broadcasting needs the rank and the strides of
both operands, and therefore needs the general layout. The framework does not
need it in stage 1: the only broadcast in the model is the bias (a vector over
the last axis) and the loss term (a scalar). The design keeps the *rule* -
align the shapes from the right, a dimension of size 1 is broadcast, the
result takes the maximum extent - and implements the two cases the model
needs as separate functions rather than one function with a mode flag, because
a mode test inside the inner loop is a branch per element and there are no
enums to switch on.

**Memory traffic.** A f32 buffer of *n* elements is 4*n* bytes, a f64 buffer is
8*n* bytes. For a forward and backward pass over a layer with *S* weights,
that is the difference between touching 4*S* bytes and 8*S* bytes per step.
At 5 GB/s that is 0.8 ms per 10^6 weights versus 1.6 ms; over a thousand
steps, a factor of two in wall clock for a memory-bound layer. This is the
first reason `f32` is not optional.

### 6.2 The autodiff

**The choice.** There are three ways to do reverse-mode differentiation in a
language without closures and without function pointers.

*Option A - store the backward function on the node.* Needs a function
pointer or a closure. The language has neither (`Ty` has no function
variant), and there is no plan to add them. **Rejected.**

*Option B - a record with a tag, dispatched in the runtime.* The runtime
would have to know the framework's node layout and the meaning of each tag.
The runtime is the memory, string, I/O and syscall layer; putting ML semantics
into it is the wrong layer, and it would make the runtime's ABI depend on the
framework. **Rejected.**

*Option C - an arena of nodes with a tag on each node, and one backward
function that dispatches on the tag.* The node holds an `Int` code; the
backward pass is a single function with a static `if`/`else` chain that calls
ordinary named functions, one per operation. **Chosen.** It is the only option
that is expressible, and it is not a compromise: it is how a tape-based
autodiff is written in any language without first-class functions.

**The tape.** Three parallel lists, threaded together through the forward and
backward passes:

```
type Node:
    kind: Int     # the tag: the op code that selects the backward
    ain: Int      # the first input node index, or -1
    bin: Int      # the second input node index, or -1
    d0: Int       # the output dimensions
    d1: Int
    aux: Int      # op-specific: the parameter index, the layer index, ...
```

```
tape:  List[Node]         # the nodes, in forward order
vals:  List[List[Float]]  # the value of each node
grads: List[List[Float]]  # the gradient of each node
```

The three lists are parallel: node *i*'s value is `vals[i]` and its gradient
is `grads[i]`. The node record holds only `Int`s, because a record cannot hold
a list; the values and the gradients are lists, and the node's *index* is
what ties them together.

`grads[i]` is created empty (of the node's output length) when the node is
pushed, and the backward pass accumulates into it. Because a list handle is a
reference, `list_get(grads, i)` yields a handle onto the same inner list, so
the accumulation is in place.

**Forward and backward.** The rule that keeps this small: **the arithmetic is
written once, functionally; the taped forward is a thin bookkeeping layer
around it.**

```
fn dense(x: List, m: Int, k: Int, w: List, b: List, n: Int) -> List
    # the arithmetic only - no tape, no node, no gradient

fn tape_dense(tape: List, vals: List, grads: List, xnode: Int,
              x: List, m: Int, k: Int, w: List, b: List, n: Int,
              aux: Int) -> Int
    # calls dense, appends the value to vals, appends a zero block of
    # length m*n to grads, appends the node to tape, and returns the
    # new node's index

fn bwd_node(tape: List, vals: List, grads: List, params: List,
            pgrads: List, i: Int)
    # the single backward function: read the node, dispatch on `kind`

fn backward(tape: List, vals: List, grads: List, params: List, pgrads: List,
            out_node: Int, y: List, nclass: Int) -> Float
    # seed the output node's gradient, then walk the tape backwards:
    #   for i = len(tape) - 1 down to 0: bwd_node(..., i)
```

The per-op backwards are ordinary functions - `bwd_dense`, `bwd_relu`,
`bwd_tanh`, `bwd_sigmoid`, `bwd_softmax_ce`, `bwd_layernorm`, `bwd_dropout`,
`bwd_embed` - called from the `if`/`else` chain in `bwd_node`. Each one reads
the node's `kind`, `ain`, `bin`, `d0`, `d1` and `aux`, and does the
arithmetic.

**The backward of a dense layer**, as the worked example:

```
y = x @ W + b        x is m x k, W is k x n, b is n, y is m x n
dL/dy                is grads[i]
dL/dx = dL/dy @ W^T  accumulates into grads[xin]
dL/dW = x^T @ dL/dy  accumulates into pgrads[aux]     (aux = the parameter index)
dL/db = sum over the rows of dL/dy
```

Everything the backward needs - the input's value (`vals[xin]`), the input's
shape (`xin.d0`, `xin.d1`), the output's gradient (`grads[i]`) - is reachable
from the node record and the input node's record. That is why the node needs
no list and no closure.

**What the tape does not do.** It does not support control flow that depends
on tensor values (no data-dependent branching), it does not checkpoint, and it
holds every intermediate value until the step ends. For an MLP that is
exactly right. For a recurrent or attention model the tape is the wrong shape
and the framework would need a different autodiff (a tape-free "one gradient
at a time" scheme, which is possible because the framework's layers are
ordinary functions - but it is out of scope here).

**The later consolidation.** In stage 5 the three parallel lists are replaced
by one flat buffer per step - the values and the gradients in one
`List[Float]`, with the nodes holding offsets instead of indices - because a
flat buffer is what the strided kernels and the GPU want. The *algorithms* do
not change: only the addressing.

### 6.3 The layers

**Dense (affine).** `y = x @ W + b`. The weight is k x n row-major, the bias is
length n. Forward and backward as above. The parameter gradient accumulates
over the batch, so it is a separate buffer per parameter tensor, not a node
gradient.

**Activations.** All of them are element-wise, so they are one pass over a
block, and their backwards are one pass over a block:

| Activation | Forward | Backward |
| ----------- | ------- | ------- |
| `relu` | `max(0, x)` | `dL/dx = dL/dy` where `x > 0`, else 0 |
| `tanh` | `(exp(2x) - 1) / (exp(2x) + 1)`, or the built-in | `dL/dx = dL/dy * (1 - t^2)` where `t` is the stored output |
| `sigmoid` | `1 / (1 + exp(-x))` | `dL/dx = dL/dy * s * (1 - s)` |
| `softmax` | per row: `exp(x - max) / sum(exp(x - max))` | fused with the loss (below) |
| `gelu` | the tanh approximation, `0.5x(1 + tanh(k(x + 0.044715x^3)))` | needs the stored output or the input |

Two rules matter numerically. First, the activation node stores its *output*,
not its input, whenever the backward can be written in terms of the output
(`tanh`, `sigmoid`): the backward then costs one multiply instead of one
transcendental. Second, the softmax subtracts the row maximum before
exponentiating; without that, `exp` overflows for any row whose maximum is
above about 88 in f64 or 88.7 in f32, and the row becomes `inf`/`nan`.

**Normalisation.** Two candidates:

* *Batch norm* normalises over the batch, so it needs two passes over the
  batch, a running mean and variance that persist across steps, and the batch
  size in the denominator. The running statistics are optimiser-like state
  that must be threaded through the training loop.
* *Layer norm* normalises each row over its own features: one pass, no
  state, no batch dependence, and the same behaviour at train and test time.

**Layer norm is the first choice**: it removes a state variable and a second
pass from the first slice, and it is the normalisation a transformer-style
model wants anyway. Batch norm is a later addition and needs the state
machinery the optimiser already has.

```
fn layernorm(x: List, m: Int, n: Int, g: List, b: List) -> List
    # per row: mean, variance, normalise, scale by g, shift by b
    # the backward needs mean, inv_std and the row's input -> stored in the node
```

**Dropout.** A mask per element, drawn from the PRNG, kept in the node's
value block (as 0.0 or 1.0), and inverted in the backward. Dropout is only
active in training mode; the mode is an `Int` parameter (`0` = inference,
`1` = training), because there are no enums and no booleans-with-meaning.
The mask must also be scaled by `1/(1-p)` at training time, or the activation
scale changes with `p`.

**Embedding.** A table of `rows x cols`; the input is a list of token ids;
the output is the gathered rows.

```
fn embed(table: List, rows: Int, cols: Int, ids: List) -> List
fn embed_grad(table: List, rows: Int, cols: Int, ids: List, dout: List) -> List
    # scatter-add: the gradient of a token that appears k times is the sum
    # of its k contributions, so the scatter must accumulate
```

The embedding is the reason the framework needs a real gather and a real
scatter-add, and it is the reason the `t_scatter_add` signature above returns
a new table rather than mutating one: the framework keeps parameters and
their gradients in separate buffers.

**Losses.**

| Loss | Forward | Backward |
| ---- | ------- | -------- |
| cross-entropy over logits | `-log softmax(y)[label]` computed as `-y + logsumexp(z)` | `p - onehot(y)` |
| mean squared error | `mean((t - y)^2)` | `2(t - y)/n` |
| binary cross-entropy over logits | `max(z,0) - z*y + log(1 + exp(-|z|))` | `sigmoid(z) - y` |
| L2 penalty | `sum(w^2)/2` | `w` |

The softmax and the cross-entropy are **fused into one op** (`softmax_ce`):
the gradient of the loss with respect to the logits is `p - onehot(y)`, and
computing it from the probabilities avoids a second exponential and a second
pass. This is the single most common optimisation in a real framework and it
is worth having from the start.

The cross-entropy must be computed as a log-sum-exp of the logits, **not** as
`log(softmax(z)[label])`. In f32, `softmax` of a 40-class problem can
underflow a component to zero, and `log(0)` is `-inf`, which turns the loss
into `nan` and the gradient into `nan`. `logsumexp` never underflows.

### 6.4 The optimisers

**State.** SGD with momentum keeps one velocity buffer per parameter tensor.
Adam keeps two (the first and second moment) plus the step counter. Because
the parameters are `List[List[Float]]` - one entry per parameter tensor - the
state is the same shape:

```
params: List[List[Float]]     # 2 entries per layer: the weights and the bias
pgrads: List[List[Float]]     # the same shape
vel:    List[List[Float]]     # SGD momentum
m:      List[List[Float]]     # Adam, first moment
v:      List[List[Float]]     # Adam, second moment
```

**SGD with momentum.**

```
fn sgd_momentum(params: List, pgrads: List, vel: List, lr: Float, mu: Float)
    # for each parameter tensor pi, for each element e:
    #   vel[pi][e] = mu * vel[pi][e] + pgrads[pi][e]
    #   params[pi][e] = params[pi][e] - lr * vel[pi][e]
```

**Adam.**

```
fn adam(params: List, pgrads: List, m: List, v: List,
        lr: Float, b1: Float, b2: Float, eps: Float, t: Int) -> Int
    # t = t + 1
    # m[pi][e] = b1 * m[pi][e] + (1 - b1) * g
    # v[pi][e] = b2 * v[pi][e] + (1 - b2) * g * g
    # mhat = m[pi][e] / (1 - b1^t)
    # vhat = v[pi][e] / (1 - b2^t)
    # params[pi][e] = params[pi][e] - lr * mhat / (sqrt(vhat) + eps)
```

Four details are not optional:

* **The epsilon goes outside the square root**: `sqrt(vhat) + eps`, not
  `sqrt(vhat + eps)`. The second form silently changes the update size for
  every parameter whose gradient is small, and it is a common bug.
* **The bias correction needs `b1^t`.** Without a `pow` builtin, keep the
  running power in the optimiser state and multiply by `b1` each step: `b1t
  = b1t * b1`. It is O(1) and exact to the same accuracy as `pow`.
* **`sqrt` must be correctly rounded.** It is in the denominator of every
  Adam update, so a systematic bias in `sqrt` biases every parameter of the
  model in the same direction. This is the one builtin where "close enough"
  is not good enough.
* **The gradient must be the true gradient of the loss as computed.** If the
  forward pass uses one `exp` and the backward pass another, the model
  optimises a slightly different function than the one whose loss it reports.
  The builtins have to be deterministic and used consistently.

### 6.5 The data pipeline

**Reading the CSV.** `read_file(path)` gives the whole file as a byte string.
The parser walks it with an index:

```
fn read_table(path: Str) -> List       # the rows, as lists of Float
fn parse_row(s: Str, start: Int, end: Int) -> List
```

The parser needs two things the language does not have:

* **a byte accessor that does not allocate.** `s[i]` allocates a one-byte
  string per byte (`crates/nexum-runtime/src/lib.rs:194-197`). The runtime
  needs `str_byte_at(s, i) -> Int`.
* **a decimal parser.** `str_to_float(s) -> Float` is needed, or the parser
  has to build the value by hand in floating point: the integer part
  accumulates exactly (`v = v * 10.0 + d`) but the fraction does not
  (`v = v + d * p` with `p = p * 0.1` accumulates about one ulp of error per
  digit). For six decimal places that is an absolute error around 1e-15,
  which is survivable for standardised features and embarrassing for
  anything else. A `str_to_float` builtin is the right answer.

The parser must also cope with the **UTF-8 BOM**: the lexer panics on one
(`crates/nexum-lexer/src/lib.rs:366`), and CSV files written by spreadsheet
applications routinely start with one. The reader has to skip it.

**Batching.** The dataset is one flat row-major buffer of `rows x cols`
floats, and the labels are a separate buffer of `rows` integers. A batch is
a contiguous run of rows, so in the stage-1 (functional) form it is a copy:
`t_copy(xs, start * cols, count * cols)`. The copy is `O(batch * cols)` and
is negligible next to the matmul it feeds. In the stage-5 (strided) form the
batch becomes an *offset* into the dataset and the copy disappears.

**Shuffling.** A permutation of the rows, applied once per epoch: the rows
are rewritten in a new order, in place, through `list_set`. The cost is
`O(rows * cols)` per epoch, which is negligible next to the matmuls.

The permutation is a Fisher-Yates shuffle driven by an integer PRNG. The
language's `Int` arithmetic wraps by definition
(`docs/language-reference/decisions.md`, D004), but a wrapped LCG can go
negative, and a negative remainder makes a negative index. The safe form
keeps the state positive and the product inside `Int`:

```
s = (s * 1103515245 + 12345) % 2147483648     # a 31-bit LCG; the product
                                                # is < 2.4e18, so it never
                                                # overflows and never wraps
j = (s / 65536) % (i + 1)                       # an Int index, no conversion
```

Note that the language has **no bitwise operators and no shifts** - the
operator list is `docs/language-reference/grammar.md:40` - so the high bits
of the state are taken with a division by a power of two rather than with a
shift. That is exact.

No `Int` to `Float` conversion is needed for the shuffle, which is why the
shuffle is in stage 1 and the weight initialisation is not (see below).

Because the features and the labels are two lists with two element types,
the shuffle has to permute both with the same permutation. The clean way is
to take both as `mut` parameters and rewrite them in place.

The seed is a literal in the source, because the language has no command-line
arguments and no clock. `fn main(a, b)` compiles, and the two parameters
receive `argc` and the `argv` *pointer* - `a` prints as `1` and `b` as a
pointer-sized integer - so there is no way to read a string out of it.
**Reproducible experiments therefore need either a command-line argument
facility or a seed constant** - the latter is acceptable, the former is what
a real framework needs.

**Weight initialisation.** Needs a normal (or uniform) draw, which needs a
`Float` from the PRNG. `Int` to `Float` has no conversion, so either the
conversion is added (it should be) or the generator works in `Float` directly:

```
s = (s * 25173.0) % 65536.0        # a float LCG whose modulus is exact
u = s / 65536.0                    # in [0, 1)
```

The Lehmer generator above is exact in f64 for the range used, because the
modulus is a power of two and the mantissa has 53 bits. It is adequate for
weight initialisation, and a proper `Int` to `Float` conversion plus a
64-bit generator would be better.

The He initialisation scale is `sqrt(2 / fan_in)`, so `sqrt` is needed on day
one.

### 6.6 The training loop

```
fn train_epoch(tape, vals, grads, params, pgrads, vel, xs, ys, rows, cols, nclass,
               widths, acts, batch, lr, mu, seed) -> Float
    # returns the mean loss over the epoch
```

The structure of an epoch:

```
perm = shuffled row order
for each batch (start, count) in the epoch:
    zero the parameter gradients
    forward: the input batch -> the layers -> the logits
    loss = the fused softmax/cross-entropy against the labels
    backward: seed the output, walk the tape backwards
    accumulate the parameter gradients
    sgd_momentum(params, pgrads, vel, lr, mu)     # or adam(...)
```

**The loss curve.** The epoch loss is pushed into a `List[Float]` and printed.
`print` takes one argument and a float prints with `%g`
(`crates/nexum-codegen/src/lib.rs:725`), i.e. six significant digits and
scientific notation when that is shorter - `6.3368e+08` for 633681729. That
is not enough to see the difference between epoch 40 and epoch 41 of a
converging model, so the curve needs either a float-to-string conversion with
enough precision or the formatter that the number-formatting work already in
flight is expected to deliver.

**Early stopping.** There is no `break`, so the epoch loop carries a flag:

```
best = 0.0
since_best = 0
stop = false
while epoch < max_epochs and not stop:
    ...
    if loss < best - min_delta:
        best = loss
        since_best = 0
    else:
        since_best = since_best + 1
        if since_best >= patience:
            stop = true
    epoch = epoch + 1
```

Every one of those float comparisons needs the compiler fix from item 1.

Note that `and` does not short-circuit
(`crates/nexum-codegen/src/lib.rs:663-664`), so the loop condition above
always evaluates `epoch < max_epochs` even once `stop` is set. That is
harmless here, but the rule has to be respected everywhere else in the
framework: an `and` whose right-hand side would divide by zero still
divides.

**Evaluation.** After the training, or after every epoch, the accuracy on a
validation split: for each row, `argmax` of the logits, compared with the
label. `argmax` needs a float comparison too. The evaluation reuses the
forward pass with dropout off and the model in inference mode.

### 6.7 The numerical requirements

**f32.** The reason is memory traffic and vector width.

* *Memory traffic.* A layer with *S* weights reads *S* values in the forward
  pass and writes *S* gradients in the backward pass, and the optimiser
  reads and writes the parameter and the two moment buffers: roughly 5*S*
  element accesses per step. At 4 bytes each that is 20*S* bytes; at 8 bytes
  it is 40*S* bytes. For a 10^7-parameter model that is 200 MB against
  400 MB per step, and at a realistic 5 GB/s that is 40 ms against 80 ms -
  before any arithmetic.
* *Vector width.* AVX2 holds 8 f32 or 4 f64 per 256-bit register; AVX-512
  holds 16 f32. The same matmul does twice the work per load in f32, and the
  ratio of loads to arithmetic is what decides whether a kernel is
  memory-bound or compute-bound.

So `f32` is a requirement, not an optimisation. The requirement on 11c is
that `f32` is a *value type* usable as a list element: the runtime's list
stores an element size and a scan size, so a 4-byte element with a zero scan
size works unchanged, and the compiler has to agree that `F32` is a value
type of size 4 with no references.

**The decision the orchestrator has to make:** does `Float` become `f32`, or
does `f32` arrive as a new type beside `Float = f64`? The existing ABI,
the existing `Float` semantics and every program written so far assume `Float`
is `f64` (`docs/runtime-abi.md:82`). The recommendation is to **keep `Float`
as `f64` and add `F32`**, and to give the framework a single name for the
tensor element type. The language has **no type aliases** (`type` declares a
record only, `docs/language-reference/grammar.md:63`), so the element type
appears literally in dozens of signatures and a change of element type is a
mechanical sweep of the whole framework. **A type alias is a prerequisite for
a cheap `f32` migration** and is on no phase's list.

**The maths builtins, and how accurate they have to be.** The rule: a
relative error *e* in a forward function produces a relative error of about
*e* in its gradient, and a systematic error (not a random one) biases every
update. The targets:

| Builtin | Target | Why |
| ------- | ------ | --- |
| `sqrt` | correctly rounded, 0.5 ulp | it is in the denominator of every Adam update and in the He scale; a bias here biases the whole model |
| `exp` | <= 1 ulp over the range that matters; correct underflow to 0 and no spurious `inf` | the softmax and the sigmoid; overflow must not be reachable after the max subtraction |
| `log` | <= 1 ulp; `log(0)` = `-inf`, `log(x<0)` = `nan`, neither traps | the cross-entropy is a log-sum-exp; a trap would kill the process instead of the batch |
| `tanh` | <= 1 ulp; `|tanh(x)| -> 1` for `|x| > 20` without overflow | the activation and its derivative `1 - t^2` |
| `erf` | <= 2 ulp absolute | only needed for the exact GELU; the tanh approximation avoids it |
| `pow` | <= 1 ulp, exact for small integer exponents | learning-rate schedules |
| `sin`, `cos` | <= 1 ulp | Box-Muller initialisation, if a proper normal generator is wanted |
| `abs` | exact | |

Two more requirements on the builtins: they must be **deterministic** (the
same input gives the same output on every call and every platform, so a loss
curve is reproducible), and they must be **available in both precisions** if
`F32` exists - a framework that trains in f32 and reports the loss in f64
needs `exp_f32` and `exp_f64` with the same relative accuracy.

**Accuracy of the training itself.**

* The **epoch loss must be accumulated in f64** even when the tensors are
  f32. Summing 10^4 per-sample losses in f32 loses about 1e-3 of relative
  precision; the reported curve would be visibly noisy at the end of
  training.
* The **gradient accumulation over a batch** in f32 is standard and
  acceptable up to a few thousand samples per batch; beyond that, accumulate
  in f64 or use compensated summation.
* The **softmax must subtract the row maximum** and the **cross-entropy must
  be a log-sum-exp**, or f32 overflows and underflows respectively.
* **NaN must be detected**, not assumed away. The check is `x != x`, which is
  a float comparison and therefore needs the compiler fix. A `nan` in the
  gradients poisons the parameters permanently, and the only cheap defence is
  to notice it in the loss and stop.
* **The forward and the backward must use the same transcendentals** (see
  6.4).

**The vectorisation precondition that is not obvious.** The tensor kernels
are loops over a `List[Float]`. For LLVM to autovectorise them it has to
prove that the stores do not alias the loads. The runtime's allocator is
declared as `declare ptr @malloc(i64)`
(`crates/nexum-runtime/src/lib.rs:52`) with no `noalias` attribute, so LLVM
must assume that a fresh buffer may alias the buffer it is writing. The
result is that the kernel is scalar even at `-O3`, and the vector width that
`f32` was supposed to buy is lost. **11a's allocator must return a `noalias`
pointer** (or the runtime must use a bump allocator whose result LLVM can
prove is fresh). This is a performance requirement with a one-line fix, and
it is easy to miss because nothing fails without it.

### 6.8 The dependency table

"Without it" means: what cannot be built, and what can still be built with
what exists today.

| Phase | What it must deliver for the framework | Without it |
| ----- | ------------------------------------- | ---------- |
| **11a** | The freestanding runtime: our own allocator, our own `exit`, our own output formatting. **The allocator must return a `noalias` pointer** so LLVM can vectorise the tensor kernels. **The allocator and the collector must be thread-safe** before 11e's threads may allocate. | Nothing blocks the framework's *semantics*: the framework compiles and runs today against the C library's `malloc` and `printf`. What is lost is the programme's goal (a binary that depends on no C library) and the vectorisation. A C-library build is a fine prototype; it is not a deliverable. |
| **11b** | Inline assembly in the language. | Nothing. The framework does not need inline assembly; it needs the *mechanism* only for the GPU and for hand-written SIMD kernels, and 11f provides a better route to both. **Not a blocker for 11i.** |
| **11c** | The numeric tower: `f32` as a value type usable as a list element; the maths builtins (`exp`, `log`, `sqrt`, `tanh`, `erf`, `pow`, `abs`, `sin`, `cos`) implemented by us; **`Int` <-> `Float` conversions in both directions**; `str_to_float`. | **The maths builtins are a hard blocker for a *real* model**: without `exp` there is no softmax and no sigmoid, without `log` there is no cross-entropy, without `sqrt` there is no He initialisation and no Adam. Without the conversions there is no `argmax`, no PRNG-to-float, and no learning-rate schedule. **But not for a prototype**: a hand-written `exp`/`log`/`tanh` in Nexum source is possible today and is enough to prove the framework end to end. Without `f32` the framework trains in f64: correct, twice the memory traffic, half the vector width. |
| **11d** | Pointers, packed records, `unsafe`, `volatile`, **explicit arenas and deterministic cleanup**. | Without pointers, a `Tensor` cannot own its buffer: a record cannot hold a `List` (`T024`), so the tensor stays a bare `List[Float]` with the shape threaded beside it. Without arenas and deterministic cleanup, the per-step activation buffer is a garbage-collected list: it is reclaimed only at a collection, so a training loop's peak memory is governed by the collector's watermark rather than by the step, and there is no way to say "this step's arena is done". Pointers are also the precondition for the GPU path (a device allocation is not a GC object). |
| **11e** | Threads, atomics, a scheduler, channels. | Optional for 11i: single-threaded training is correct, just slower. The design is concurrency-ready - the tape, the values and the gradients are per-step state passed as parameters, so each thread would own its own - **but only if 11a's allocator and the collector are thread-safe first**. A multithreaded data loader (parse the next batch while the current one trains) is the cheapest win here. |
| **11f** | Vectors, strides, intrinsics, explicit layout. | Optional for correctness; it is the difference between "works" and "fast", and it is what makes the *general* tensor possible: with strides, a transpose is a view and not a copy, a slice is a view, and the broadcasting rule becomes expressible. Without it, the framework keeps the flat stage-1 kernels, and a transpose copies. |
| **11g** | The native linker. | Nothing for 11i. The framework does not care who links. |
| **11h** | The GPU path: a driver-native compute path. | Optional; without it the framework trains on the CPU. The *design* has to anticipate it: a tensor must be able to be a device handle, so the tensor record needs a pointer and a memory-space tag - which needs **11d**. The autodiff does not change: the same node tags dispatch to a CPU kernel or a compute shader. |

**And three items that are on no phase's list.** They are listed here because
the framework is what makes them matter, and the orchestrator has to place
them:

1. **Float comparison in the code generator** (a defect, section "the
   compiler defect"). Everything else can wait; this cannot.
2. **`List[T]` in the type grammar**, so that a record can hold a typed list
   (`crates/nexum-sema/src/lib.rs:537-555` and the T049 relaxation at
   `crates/nexum-sema/src/lib.rs:1399-1419`). Without it the framework's
   central type does not exist and the model is five parallel lists instead
   of one value.
3. **Type aliases**, so that changing the tensor element type from `f64` to
   `f32` is a one-line change rather than a sweep of every signature.

Two smaller ones, both cheap: **`str_byte_at(s, i) -> Int`** (the CSV reader
allocates a string per byte without it) and **float-to-string with more than
six significant digits** (the loss curve is unreadable without it).

### 6.9 The staged plan

**Stage 0 - unblock the compiler.** Fix the float comparison (item 1 of the
Recommendation). Add a regression test that compiles and runs `if 1.0 < 2.0`.
Nothing else can start without this.

**Stage 1 - the tensor core, the data pipeline and a training loop, in f64.**
A flat `List[Float]` tensor; the element-wise kernels, the broadcast, the
reductions, the matmul, the transpose; a hand-written `exp`, `log`, `tanh`,
`sqrt` in Nexum source (they are replaced in stage 4); the CSV reader; the
shuffle; the epoch loop; the loss curve; the evaluation; and - to prove the
loop before there is an autodiff - a linear model with its gradient written
out by hand. **Deliverable:** a logistic regression trains on a small tabular
CSV and the loss goes down, and the tensor core is proved by the kernels'
own tests.

**Stage 2 - the autodiff.** The node record, the tape, the parallel value and
gradient lists, the single
backward function and the per-op backwards. **Deliverable:** a two-layer MLP
trains, with the gradients computed by the tape rather than by hand, and a
gradient check (a finite difference comparison against the analytic
gradient) that the framework can run on demand.

**Stage 3 - the full layer set and the optimisers.** Dense, relu, tanh,
sigmoid, softmax+CE fused, layer norm, dropout, embedding; MSE and L2;
SGD with momentum and Adam; the early stopping.

**Stage 4 - the numeric tower.** When 11c lands: replace the hand-written
maths with the builtins, move the tensors to `f32`, keep the loss
accumulation in f64. This stage is a *replacement*, not a rewrite, because
the algorithms are layout- and precision-independent.

**Stage 5 - the general tensor.** When 11d and 11f land: the `Tensor`
record (a pointer plus a layout), the strided kernels, the views (a
transpose and a slice stop copying), the broadcasting rule in its general
form, and the vectorised matmul with accumulator blocking. The stage-1
kernels are replaced by the strided ones; the layers, the tape, the
optimisers and the training loop do not change.

**Stage 6 - concurrency.** When 11e lands (and 11a's allocator is
thread-safe): a parallel data loader, and data-parallel gradient accumulation
over batches.

**Stage 7 - the GPU path.** When 11h lands: the tensors become device
handles, the kernels become compute shaders, and the tape learns to hold
device-resident values. The dispatch in `bwd_node` is unchanged; only the
kernel behind each tag changes.

### 6.10 Stage 1: the first slice, specified

**Files** (in a `ml/` directory, imported by `main.nx` with
`import "ml/tensor.nx"`; no file but `main.nx` defines `main`):

| File | Contents |
| ---- | -------- |
| `ml/tensor.nx` | the tensor kernels |
| `ml/math.nx` | the hand-written `exp`, `log`, `tanh`, `sqrt` (removed in stage 4) |
| `ml/rng.nx` | the PRNG and the initialisers |
| `ml/data.nx` | the CSV reader, the standardiser, the shuffler |
| `ml/model.nx` | the stage-1 linear model, with its gradient written out by hand (replaced by the taped layers in stage 2) |
| `ml/train.nx` | the epoch loop, the loss curve, the evaluation |
| `main.nx` | the program: read the CSV, build the model, train, report |

**The tensor core** (`ml/tensor.nx`):

```
fn t_zeros(n: Int) -> List
fn t_fill(n: Int, v: Float) -> List
fn t_copy(src: List, off: Int, len: Int) -> List
fn t_add(a: List, b: List) -> List
fn t_sub(a: List, b: List) -> List
fn t_mul(a: List, b: List) -> List
fn t_scale(a: List, s: Float) -> List
fn t_add_vec(a: List, m: Int, n: Int, v: List) -> List
fn t_sum(a: List) -> Float
fn t_mean(a: List) -> Float
fn t_max(a: List) -> Float
fn t_argmax(a: List) -> Int
fn t_sum_rows(a: List, m: Int, n: Int) -> List
fn t_max_rows(a: List, m: Int, n: Int) -> List
fn t_matmul(a: List, m: Int, k: Int, b: List, n: Int) -> List
fn t_matmul_t(a: List, m: Int, k: Int, b: List, n: Int) -> List
fn t_transpose(a: List, m: Int, n: Int) -> List
fn t_softmax_rows(a: List, m: Int, n: Int) -> List
```

`fn t_matmul(a: List, m: Int, k: Int, b: List, n: Int) -> List` is the
reference implementation:

```
fn t_matmul(a: List, m: Int, k: Int, b: List, n: Int) -> List:
    # c is m x n, row-major; a is m x k, b is k x n
    let mut c = list_new()
    let mut i = 0
    while i < m:
        let mut j = 0
        while j < n:
            let mut s = 0.0
            let mut l = 0
            while l < k:
                s = s + list_get(a, i * k + l) * list_get(b, l * n + j)
                l = l + 1
            list_push(c, s)
            j = j + 1
        i = i + 1
    return c
```

Note the element type discipline: `c` is `List[Float]` because the first
`list_push` pushes a `Float`, and the compiler fixes the element type there.
`a` and `b` are `List[Float]` too, and the compiler infers that from the
call sites, so the kernels need no type annotations at all.

**The maths** (`ml/math.nx`, replaced in stage 4):

```
fn m_exp(x: Float) -> Float
fn m_log(x: Float) -> Float
fn m_tanh(x: Float) -> Float
fn m_sqrt(x: Float) -> Float
fn m_pow(x: Float, n: Int) -> Float
```

`m_sqrt` is Newton's method on `x * s = v`; `m_exp` is argument reduction
(`x = k*ln2 + r`) plus a Taylor series on `r`; `m_log` is the same reduction
with an atanh series; `m_tanh` is `m_exp` of `2x`. These are slow and only
about 1e-12 accurate, which is enough to prove the framework and not enough
to publish.

**The PRNG and the initialisers** (`ml/rng.nx`):

```
type Rng:
    state: Int
    fstate: Float

fn rng_new(seed: Int) -> Rng
fn rng_next(mut r: Rng) -> Int
fn rng_uniform(mut r: Rng) -> Float
fn init_weights(rows: Int, cols: Int, seed: Int) -> List
fn init_biases(cols: Int) -> List
```

The `mut` on the parameter is not decoration. A record is a value type, so a
plain parameter is a copy and the caller's state would never advance; a `mut`
parameter borrows the caller's binding and the assignments inside the callee
are visible in the caller afterwards (D001). The call site must therefore pass
a `let mut` binding, and T037 rejects anything else - a record field, for
instance, which is why the whole generator is one record and not a pair of
separate counters.

`rng_uniform` is the float LCG of section 6.5, which avoids the missing
`Int` to `Float` conversion. `init_weights` uses the He scale
`sqrt(2.0 / fan_in)`, so it needs `m_sqrt`.

**The data pipeline** (`ml/data.nx`):

```
fn d_read_csv(path: Str) -> Str
fn d_parse_floats(s: Str, start: Int, end: Int) -> List
fn d_parse_row(s: Str, start: Int, end: Int) -> List
fn d_standardise(xs: List, rows: Int, cols: Int) -> List
fn d_shuffle(mut xs: List, mut ys: List, rows: Int, cols: Int, seed: Int)
fn d_permutation(rows: Int, seed: Int) -> List
```

`d_read_csv` is `read_file`. `d_parse_row` walks the bytes between two
offsets and pushes the fields; it needs `m_parse_float(s, lo, hi) -> Float`
(the hand-written decimal parser) until `str_to_float` exists.

The features and the labels are two separate lists, because their element
types differ: the features are `List[Float]` and the labels are `List[Int]`,
and an `Int` cannot be compared with a `Float` (`T008`), so the label of a
row and the `argmax` of its logits have to be the same type. Shuffling two
lists consistently is the awkward consequence, and `d_shuffle` resolves it by
taking both as `mut` parameters and permuting them in place with one
permutation - `mut` parameters borrow the caller's binding, so the caller's
lists are rewritten without a copy (`d_permutation` is exposed separately for
the case where the caller wants the order).

The validation split needs no function: it is a rule on the row index (every
*k*-th row is a test row), which keeps the features and the labels in step
for free.

**The training loop** (`ml/train.nx`):

```
fn tr_forward(x: List, m: Int, nin: Int, params: List, widths: List,
              acts: List, nclass: Int, y: List) -> Float
    # the forward pass and the loss; the layer loop is over `widths`

fn tr_epoch(xs: List, ys: List, rows: Int, cols: Int, nclass: Int,
            params: List, widths: List, acts: List, batch: Int,
            lr: Float, mu: Float, seed: Int) -> Float

fn tr_train(xs: List, ys: List, rows: Int, cols: Int, nclass: Int,
            params: List, widths: List, acts: List, batch: Int,
            epochs: Int, lr: Float, mu: Float, patience: Int,
            seed: Int) -> List
    # returns the loss curve, one Float per epoch

fn tr_accuracy(xs: List, ys: List, rows: Int, cols: Int, nclass: Int,
               params: List, widths: List, acts: List) -> Float
```

In stage 1 the model is the linear one in `ml/model.nx`, with its gradient
written out by hand; the loop over `widths` and `acts` - one entry per layer
- arrives with the taped layers in stage 2, and the parameter list becomes
two entries per layer (the weights and the bias) at the same time.

**The program** (`main.nx`):

```
import "ml/data.nx"
import "ml/train.nx"

fn main():
    let raw = read_file("data/train.csv")
    let xs = ...            # rows x cols, standardised
    let ys = ...            # one label per row
    let params = ...        # List[List[Float]]: 2 entries per layer
    let widths = ...        # List[Int]
    let acts = ...          # List[Int]: the activation code per layer
    let curve = tr_train(xs, ys, rows, cols, nclass, params, widths, acts,
                          32, 200, 0.01, 0.9, 20, 12345)
    let acc = tr_accuracy(xs, ys, rows, cols, nclass, params, widths, acts)
    print(acc)
```

**What "done" means for stage 1:** the program compiles, it reads a CSV, it
prints a loss per epoch, the loss decreases by more than a factor of two over
the run, and the reported accuracy on the training set is above the majority
class. A gradient check is stage 2.

### 6.11 The honest scope

**What the framework is, at the end of 11i:** a multilayer perceptron on a
tabular dataset, trained end to end, with a tape-based autodiff, six
activations, layer norm, dropout, an embedding layer, two losses, two
optimisers, a CSV data pipeline, a loss curve, early stopping and an
evaluation. That is a real framework and it is what the language can carry.

**What it is not:** there is no convolution (that is a rank-4 tensor and a
gather-and-multiply kernel), no attention, no recurrent layers, no
checkpointing, no data-parallel training until 11e, no GPU until 11h, and no
model zoo.

**What images would need, on top of what exists:**

1. **A fourth dimension.** A tensor of shape N x C x H x W needs the rank-4
   shape and the general stride arithmetic. Today the shape is a set of
   `Int` parameters, so a rank-4 kernel is possible, but the code stops
   being a list of rank-2 kernels.
2. **Strided views.** A random crop is a view, not a copy; with the flat
   representation every crop copies the whole image. This needs 11f.
3. **Convolution.** Expressed as im2col plus a matmul, the im2col being a
   gather with a computed index - it needs the strided addressing of 11f and
   the vectorised matmul of 11f.
4. **Binary file input.** The image loader reads bytes, and `read_file`
   returns a NUL-terminated string, so a PNG cannot be read at all
   (`docs/runtime-abi.md:232-233`). **Binary I/O is a hard requirement for
   images and does not exist yet.** It belongs to 11a/11d.
5. **Batch norm over the spatial dimensions**, with the running statistics -
   more state than layer norm, and the reason the optimiser's state pattern
   has to be generalised.
6. **The GPU path**, for anything but a toy model. 11h.

The honest summary: **the MLP is the honest scope of 11i; images are 11h plus
a binary-I/O facility that no phase currently owns.**

## Risks

* **The float comparison fix is small and might be assumed rather than
  scheduled.** If it is not fixed, every estimate in this document is wrong,
  because nothing compiles. The risk is that it is discovered late, after a
  framework has been written against a compiler that rejects float
  comparisons. **What would have to be true for the recommendation to be
  wrong:** that a float comparison can be expressed today by some other
  route. There is none: `==` and `!=` are broken as well as the orderings.
* **The `T049` element-type rule may be deeper than it looks.** The
  framework assumes that a list's element type is inferred correctly
  through function boundaries - `fn batch(xs: List, ...) -> List` returning a
  `List[Float]`. That works today (the inference is a fixpoint,
  `docs/language-reference/grammar.md:113-116`), but it is a *monomorphic*
  inference over a program-wide namespace: two functions that return lists of
  different element types and are called from the same place will collide.
  The framework avoids this by keeping every list in the program a
  `List[Float]` except the explicitly `List[Int]` ones (the widths, the
  activation codes, the row indices), and by never passing a `List[Int]` and
  a `List[Float]` through the same inference site. **What would have to be
  true for the recommendation to be wrong:** that the inference cannot
  distinguish two list-returning functions with different element types. The
  first slice should therefore be small enough that this can be checked
  quickly.
* **The collector may dominate the run time.** Every step allocates a fresh
  value list and a fresh gradient list for every node, and the batches are
  copied out of the dataset. A `Float` buffer is not scanned, so the marking
  is cheap, but the *allocation* and the *doubling copy* of the growing
  buffers are not. A 100-step run whose per-step buffers total 1 MB
  allocates 100 MB and copies about 200 MB. If this shows up, the fix is
  11d's arenas (a bump allocator with one release point per step), not a
  redesign.
* **The scalar kernels will be latency-bound, not throughput-bound.** The
  measured 1.5 ns per dependent float add says the matmul's inner loop is
  waiting on the accumulator. Accumulator blocking (four accumulators) would
  help immediately and costs nothing; 11f's vectors help more. **What would
  have to be true for the recommendation to be wrong:** that LLVM's `-O3`
  already restructures the loop - it does not, because the accumulator is a
  single SSA value that every iteration must extend.
* **The optimiser's state shape may be wrong.** The design keeps the state as
  `List[List[Float]]` parallel to the parameters. That is fine for a model
  whose parameters are a fixed list of tensors, and wrong for a model whose
  parameter count varies (a mixture of experts, a LoRA adapter added at
  runtime). It is also wrong for a framework that wants to serialise the
  optimiser state - which is a `List[List[Float]]` and no problem at all.
* **The `noalias` point may be wrong.** If LLVM can already prove
  non-aliasing for the list buffer (because `nexum_alloc` is `internal` and
  the buffer is a fresh allocation), the vectorisation problem does not
  exist. **What would have to be true:** that the emitted IR for the matmul
  contains vector instructions at `-O3`. That is a five-minute check with
  `nx emit-ir`, and it should be done before 11a is scheduled.

## Open questions

Each of these is a decision the orchestrator has to make.

1. **Does `Float` become `f32`, or does `f32` arrive as a new type?**
   *Recommendation:* keep `Float` as `f64` (the ABI, the existing semantics
   and every program written so far depend on it) and add `F32` beside it.
   Then add type aliases, so the tensor element type is one name.
2. **Who owns the float comparison fix, and when?**
   *Recommendation:* treat it as a defect on the critical path for 11i, not
   as a phase. It is a handful of lines in the code generator plus a
   regression test. Nothing else in this document can start without it.
3. **Who owns `List[T]` in the type grammar?**
   *Recommendation:* a small front-end change, taken before the framework
   starts, so that the framework is written against a `Tensor` record rather
   than against five parallel lists. Without it the framework's public API is
   unusable by anyone else.
4. **Is the tensor element type f32 from the first slice, or f64 until 11c?**
   *Recommendation:* f64 from the first slice, with the element type appearing
   in exactly one place per signature and no type alias available - which is
   another argument for the alias. If the alias lands first, the migration
   is a one-line change.
5. **Does the framework own the CSV reader, or is the CSV reader a separate
   library?** The reader needs `str_to_float` and a non-allocating byte
   accessor, which are runtime builtins; the parsing itself is ordinary
   library code. *Recommendation:* the runtime gets the two builtins, the
   framework ships the CSV reader as part of the data pipeline, and the
   number formatting is delivered by the number-formatting work already in
   flight.
6. **How are the hyper-parameters supplied?** There are no command-line
   arguments and no environment access. *Recommendation:* stage 1 takes them
   as constants in `main.nx`, and the orchestrator decides separately whether
   to add an `argv`/`args` facility. A framework that cannot be pointed at a
   dataset from the command line is a framework nobody will use twice.
7. **Is the loss curve printed or written to a file?**
   `write_file` exists, so a CSV of the curve is one line of code - and it
   avoids the float-to-string precision question for the *stored* curve,
   though not for the printed one. *Recommendation:* write the curve to a
   file in stage 1 and print a summary; the precision question then only
   affects what the user sees.
8. **Does 11i's framework need to be a *package*?** The package format is
   documented (`docs/package-format.md`) and the module system is one
   namespace with `import`. The framework will be several files; whether it
   is one package or a set of imported files is a distribution question, not
   a design one. *Recommendation:* plain imported files for now.

## Changes needed elsewhere

The framework needs one change outside this document, in code the orchestrator
owns. It is a defect, and it is recorded here rather than fixed, because this
task is research.

**`crates/nexum-codegen/src/lib.rs:615-662` - float comparisons emit
`icmp` on doubles.** The comparison's *result* type is `Bool`
(`crates/nexum-sema/src/lib.rs:2017-2043`), so the `*ty == Ty::Float` test at
line 624 is never true, and the ordering operators at lines 651-662 have no
float branch at all. The result is invalid LLVM IR:

```
error: icmp requires integer operands
  %v2 = icmp eq double %v0, %v1
```

The fix is to branch on the operand type - `oty_str` (already computed at
line 519) or the operand's `Ty` from `vt` - and to emit `fcmp oeq`, `fcmp
one`, `fcmp olt`, `fcmp ogt`, `fcmp ole`, `fcmp oge` for `Float` operands.
`Eq` and `Ne` need the same treatment as the four orderings. A regression
test should compile and run a program that compares two floats with each of
the six operators.

Two smaller items, in the same spirit:

* **`crates/nexum-sema/src/lib.rs:537-555` and `:1399-1419`** - `List` as a
  record field type, with a declarable element type (`List[Float]`), so that
  a tensor can be one value. This is the change that unblocks the framework's
  public shape.
* **`crates/nexum-runtime/src/lib.rs:52`** - `noalias` on the allocator's
  return, so that LLVM can vectorise the tensor kernels. Performance only,
  but it decides whether `f32` buys anything.

## Appendix: the compiler probes

Every "verified" claim above comes from one of these, run with the
prebuilt `nx.exe` at `target/debug/nx.exe` in this worktree.

| Probe | What it shows | Result |
| ----- | ------------- | ------ |
| `t1.nx` | a record field typed `List` | `error[T024]: unknown type `List` for field `data`` |
| `t2.nx` | an unannotated parameter that is a list; `list_get` on a `List[Float]` yields a `Float` | check: ok; `count(a)` = 1, `list_get(a,0)` = 1.0 |
| `t3.nx` | records, field access, field assignment on a `let mut` binding | check: ok |
| `t4.nx` | a `List` of `List` | check: ok |
| `t5.nx` | `fn count(l: List)` as a parameter annotation | check: ok (it resolves to `Unknown`, which is compatible with everything) |
| `t6.nx` | a function returning a list; the element type propagates to the caller | check: ok |
| `t7.nx` | `for v in l` over a `List` | `error[T038]: for-each requires an array, found List[Pair]` |
| `t8.nx` | a top-level `let` used inside a function | `error[T004]: undefined variable `g`` - **no globals** |
| `t9.nx` | a `mut` parameter growing a list in place | check: ok; the caller's list grows |
| `t10.nx` | `list_get` copies a record element; a list handle is shared | runs; `q.x = 99` did not change the stored element, and `list_push(list_get(outer,0), 6.0)` grew the shared inner list |
| `t11.nx` | a float-to-int conversion | `error[T006]: undefined function `Int`` - **no conversions** |
| `t12.nx` | float arithmetic, `inf` and `nan` do not trap; `len` on a string | runs; `2.5`, `inf`, `nan`, `5` |
| `t13.nx`, `t16.nx` | `main` may take parameters | runs; `a` = 1 (`argc`), `b` = the `argv` pointer as an integer - **no argv as strings** |
| `t14.nx` | `if v > best` on floats | `error: icmp requires integer operands` |
| `t15.nx` | `if 1.0 == 1.0` | `error: icmp requires integer operands` |
| `t17.nx` | `import "lib1.nx"` | runs; the imported function is callable (a library file must not define `main`) |
| `t18.nx` | a `mut` parameter of a record type, whose fields the callee assigns | runs; the caller's record is updated (`r.state` = 2 after two calls) - this is how a PRNG advances |
| `bench.nx` | 10^6 multiply-adds, each with a `list_get` | ~12-18 ms in a `--release` build |
| `bench3.nx` | 10^7 dependent float additions | ~15 ms of work |

### The probes themselves

The three that matter most, in full:

```
# t1.nx -- a record field typed List
type Tensor:
    data: List
    shape: Int

fn main():
    let mut t = Tensor(list_new(), 0)
    print("built")
```

```
error[T024]: unknown type `List` for field `data`
  --> t1.nx:1:1
```

```
# t15.nx -- float equality
fn main():
    if 1.0 == 1.0:
        print("eq works")
```

```
C:\nexum_out.ll:623:17: error: icmp requires integer operands
  623 |   %v2 = icmp eq double %v0, %v1
```

```
# t10.nx -- the copy and reference rules
type Pair:
    x: Int
    y: Int

fn main():
    let mut l = list_new()
    list_push(l, Pair(1, 2))
    list_push(l, Pair(3, 4))
    let mut q = list_get(l, 0)
    q.x = 99
    print(list_get(l, 0).x)          # prints 1: the record was copied
    let mut outer = list_new()
    let mut inner = list_new()
    list_push(inner, 5.0)
    list_push(outer, inner)
    list_push(list_get(outer, 0), 6.0)
    print(list_len(list_get(outer, 0)))   # prints 2: the handle is shared
```

```
# t8.nx -- no globals
let mut g = list_new()

fn main():
    list_push(g, 1.0)
```

```
error[T004]: undefined variable `g`
```

The rest are in the table above and are one or two lines each. Every probe
was run with the compiler at `target/debug/nx.exe` at commit `316f6c6`; the
numbers in the "measured behaviour" section come from the same compiler with
`--release` (clang `-O3`, thin LTO) on an x86-64 Windows host.
