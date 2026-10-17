# Nexum Package Format (Phase 9)

## Project layout

```
myproject/
  nexum.toml          # manifest
  src/
    main.nx           # entry point (default)
  tests/              # `nx test` runs *.nx here
  examples/           # optional, IR-verified by `nx` tooling
  docs/               # optional
  nexum.lock          # generated lockfile (future)
```

## Manifest (`nexum.toml`)

```toml
[package]
name = "myproject"
version = "0.1.0"
edition = "2026"
entry = "src/main.nx"
```

| Field     | Required | Description                                              |
| --------- | -------- | -------------------------------------------------------- |
| `name`    | yes      | Package name (lowercase, `[a-z0-9_-]+`).                 |
| `version` | yes      | SemVer version of the package.                           |
| `edition` | no       | Language edition (default `2026`).                        |
| `entry`   | no       | Entry source file (default `src/main.nx`).                |

`nx run` / `nx build` / `nx check` with no file argument read `nexum.toml`
from the current directory and use `entry`.

## Lockfile (`nexum.lock`)

Reserved for Phase 9+ (dependency resolution). The bootstrap does not yet
support external dependencies; the lockfile format is published here for
forward compatibility:

```toml
# nexum.lock (v1)
version = 1
[[package]]
name = "..."
version = "..."
source = "..."
checksum = "..."
```

## Editions

The only supported edition is `2026`. Unknown editions are rejected by
`nx` with an error. New editions are introduced by a future language
release and selected via the manifest.

## Creating a project

```
nx new myproject
cd myproject
nx run          # uses nexum.toml entry
nx test         # runs tests/*.nx
nx build --release
```