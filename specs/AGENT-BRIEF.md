# Agent brief — the native roadmap programme

Status: **active**. This brief applies to every agent dispatched for the
native roadmap (phase 11 and beyond). Read it, then read
`docs/native-roadmap.md`, then do your task.

## Why the programme exists

Nexum is moving from a general purpose language to one that owns the
machine: a freestanding runtime, a standard library written in Nexum, and
no C library in between. The phases are in `docs/native-roadmap.md`.

## The rules

1. **Your context is isolated.** You cannot see what other agents did. The
   only bridge is files. Read the files named in your task before you
   start, and write your result to the file named in your task.
2. **Write only the file you were told to write.** Other agents are
   working in the same repository at the same time. Touching a file that is
   not yours will lose work. If you need a change somewhere else, write it
   in your own file under "Changes needed elsewhere".
3. **Work in your own worktree, on your own branch, and submit a pull
   request.** Many agents run at the same time and they share one working
   tree, so you must not work in the main checkout. The sequence is:

   ```powershell
   # from C:\Nexum\nexum
   git worktree add C:\Nexum\work\<name> -b <your-branch-name> main
   cd C:\Nexum\work\<name>
   # ... do the work, in this directory ...
   git add <only your files>
   git commit -m "<type>: <what and why>"
   git push -u origin <your-branch-name>
   gh pr create --base main --head <your-branch-name> --title "<title>" --body "<body>"
   ```

   Your branch name, worktree directory and pull request title are given
   in your task. The body is the "Recommendation" of your document in a
   few paragraphs: what you concluded, what the orchestrator has to
   decide, and what you could not verify. The orchestrator reviews the
   pull request and merges it.

   Do not create a worktree for a branch that already exists, and do not
   remove a worktree that is not yours.
4. **Do not commit files that are not yours.** `git add` exactly the files
   you were told to write. If the working tree has other changes, leave
   them alone.
5. **Do not reformat, do not fix unrelated things, do not change branches
   other than your own.** The orchestrator commits and merges.
6. **Do not remove or weaken existing tests.** If a test fails because of
   your change, fix the change.
7. **No new dependencies.** The toolchain is Rust with the crates it
   already has; the language's programs must not need the C library.
8. **Verify before you report.** Run the checks your task names and paste
   the real output in your report. If you cannot run them, say so and say
   why. Never report work as passing that you did not run.
9. **Report honestly.** Your report states what you changed, what you
   verified, and what is still missing. A failure you found and explained
   is worth more than a success you cannot back up.

## The shape of a proposal

Every research deliverable is a markdown file under `specs/research/` with
these sections, in this order:

```markdown
# <title>

**Question.** The question this document answers.

**Summary.** Three to five sentences. The recommendation comes first.

## Recommendation

The concrete recommendation, numbered, with the reason for each choice.

## What the code looks like today

The facts from the repository, with `file:line` references. Only what the
recommendation depends on.

## The design

The design itself: the syntax, the data structures, the lowering, the
layout. Concrete enough that an implementer does not have to guess.

## Risks

What could go wrong, and what would have to be true for the recommendation
to be wrong.

## Open questions

The questions the orchestrator has to decide, with a recommendation for
each.
```

## The shape of an implementation deliverable

Every implementation writes the code **and** a short report at the path
its task names, with these sections:

```markdown
# <what was implemented>

**Changed.** The files, and what each one does.

**Verified.** The commands that were run and their real output.

**Not done.** What is still missing, and why.

**Changes needed elsewhere.** Anything the orchestrator has to wire up.
```

## The house rules for the language and the runtime

* The runtime is freestanding on the POSIX targets: no C library
  (`docs/runtime-abi.md` §0). The Windows build is still on the C library,
  because Windows has no direct system call instruction.
* Records cannot contain themselves, so an arena of nodes with indices is
  the shape for any tree the compiler or the runtime builds
  (`docs/language-reference/decisions.md`, D009).
* `Str` and `List` are the only managed types. A `List`'s element type is
  fixed by its first push (T049).
* `and` and `or` do not short-circuit.
* Strings are byte strings.
* The collector is a non-moving mark-sweep collector; a program cannot
  free deterministically yet, and arenas are staged (D009).
* Every decision that changes the language gets a numbered entry in
  `docs/language-reference/decisions.md`, and the grammar changes get the
  matching change in `docs/language-reference/grammar.md`.
