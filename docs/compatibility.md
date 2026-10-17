# Nexum Compatibility and Migration (Phase 9)

## Language editions

The language is versioned by **edition**. The only supported edition today is
`2026`. An edition is selected in `nexum.toml`:

```toml
[package]
edition = "2026"
```

A program without a manifest defaults to the latest stable edition.

## SemVer policy

* **Patch releases** (0.x.y): bug fixes, no language changes.
* **Minor releases** (0.x.0): additive changes only — new builtins, new
  syntax that does not break existing programs.
* **Major releases** (x.0.0) / **new editions**: may change semantics. Old
  editions remain buildable for at least one major release cycle.

## Deprecation

Deprecated constructs emit a warning (not an error) for one full release
cycle before removal. Removals are announced in `CHANGELOG.md` and the
migration guide.

## Migration process

1. Run `nx doctor` to check the toolchain and current edition.
2. Run `nx check` to surface new diagnostics introduced by a newer edition.
3. Update `edition` in `nexum.toml`.
4. Fix diagnostics; `nx fmt` applies canonical formatting.
5. Run `nx test` and the differential suite before releasing.

## Runtime ABI compatibility

The runtime ABI is versioned independently of the language (see
`runtime-abi.md`). A breaking ABI change requires a major version bump of
the runtime and a documented migration. Programs compiled against an older
ABI are not guaranteed to link against a newer runtime.