//! The data layouts must be the ones clang computes.
//!
//! `TargetSpec::data_layout_str` is what the toolchain writes into
//! `target datalayout = "..."` in every module it emits. When it
//! disagrees with clang's layout for the same triple, the backend lays
//! the data out differently from the way the C compiler lays it out for
//! the same target, and nothing in the build reports it.
//!
//! There are two checks here:
//!
//! * `data_layouts_match_the_canonical_layouts` pins the five layouts to
//!   the strings clang computes for them. It always runs, on a machine
//!   with no LLVM at all.
//! * `data_layouts_agree_with_the_installed_clang` asks the clang that is
//!   installed, if there is one, and compares. It is skipped when there
//!   is no clang on the PATH.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

use nexum_target::TargetSpec;

/// The layout clang computes for each target.
///
/// `clang -target <triple> -S -emit-llvm -x c - -o -` prints
/// `target datalayout = "..."`. The strings below are the ones clang
/// 23.1.3 computes, and they have been the upstream ones since LLVM 20:
/// LLVM 18 added `i128:128` to the x86 layouts (llvm-project a21abc7),
/// LLVM 19 added `Fn32` to the AArch64 ones, and LLVM 20 added the
/// `p270`, `p271` and `p272` address spaces to the AArch64 ones.
const CANONICAL: &[(&str, &str)] = &[
    (
        "windows-x86_64",
        "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    ),
    (
        "linux-x86_64",
        "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    ),
    (
        "linux-aarch64",
        "e-m:e-p270:32:32-p271:32:32-p272:64:64-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128-Fn32",
    ),
    (
        "macos-x86_64",
        "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-f80:128-n8:16:32:64-S128",
    ),
    (
        "macos-aarch64",
        "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32",
    ),
];

/// The specs upstream LLVM added to a layout after the oldest clang this
/// project supports (LLVM 17), with the value upstream gives them.
///
/// A layout that is ahead of the installed clang by one of these is not
/// drift: it is the newer canonical layout, and the installed clang is
/// simply older. A layout that is *behind* the installed clang is drift,
/// and the check below rejects it.
const SPECS_ADDED_UPSTREAM: &[(&str, &str)] = &[
    // LLVM 18 (llvm-project a21abc7) added i128:128 to the x86 layouts.
    ("i128", "128"),
    // LLVM 19 added Fn32 to the AArch64 layouts.
    ("Fn32", ""),
    // LLVM 20 added the p270, p271 and p272 address spaces to the
    // AArch64 layouts.
    ("p270", "32:32"),
    ("p271", "32:32"),
    ("p272", "64:64"),
];

/// The clang to ask. The plain name comes first, because that is the one
/// the toolchain itself uses; the versioned names are the fallback for a
/// machine that has several LLVM installs and no unversioned one.
const CLANG_NAMES: &[&str] = &[
    "clang", "clang-21", "clang-20", "clang-19", "clang-18", "clang-17",
];

#[test]
fn data_layouts_match_the_canonical_layouts() {
    for (name, expected) in CANONICAL {
        let spec = TargetSpec::by_name(name).unwrap_or_else(|| panic!("{name}: unknown target"));
        assert_eq!(
            spec.data_layout_str(),
            *expected,
            "{name}: the data layout drifted from the one clang computes"
        );
    }
}

/// Every target the toolchain can build for needs a canonical layout, or
/// it falls back to a layout that is not its own.
#[test]
fn every_target_has_a_canonical_layout() {
    let mut listed: Vec<&str> = TargetSpec::list().to_vec();
    let mut pinned: Vec<&str> = CANONICAL.iter().map(|(name, _)| *name).collect();
    listed.sort_unstable();
    pinned.sort_unstable();
    assert_eq!(
        listed, pinned,
        "the target list and the canonical layouts are not the same set"
    );
}

/// The host must get the layout of its own target, not the fallback one.
#[test]
fn the_host_gets_the_layout_of_its_own_target() {
    let host = TargetSpec::host();
    let name = match (host.arch.as_str(), host.os.as_str()) {
        ("x86_64", "windows") => "windows-x86_64",
        ("x86_64", "linux") => "linux-x86_64",
        ("x86_64", "macos") => "macos-x86_64",
        ("aarch64", "linux") => "linux-aarch64",
        ("aarch64", "macos") => "macos-aarch64",
        // A host this project does not target yet: the fallback layout is
        // all it can get.
        _ => return,
    };
    let target = TargetSpec::by_name(name).unwrap();
    assert_eq!(
        host.data_layout_str(),
        target.data_layout_str(),
        "the host ({}) does not get the layout of {name}",
        host.triple
    );
}

/// The layouts must agree with the ones the installed clang computes.
///
/// This is the check that would have caught the drift: the hand-written
/// layouts were missing `i128:128`, the `p270`/`p271`/`p272` address
/// spaces and `Fn32`.
#[test]
fn data_layouts_agree_with_the_installed_clang() {
    let Some(clang) = find_clang() else {
        // No clang on the PATH: the pinned layouts are the check there.
        return;
    };
    for (name, _) in CANONICAL {
        let spec = TargetSpec::by_name(name).unwrap_or_else(|| panic!("{name}: unknown target"));
        let clang_layout = clang_layout(&clang, &spec.triple).unwrap_or_else(|| {
            panic!(
                "{name}: clang could not report the layout of {}",
                spec.triple
            )
        });
        assert_layouts_agree(name, spec.data_layout_str(), &clang_layout);
    }
}

/// Find a clang to ask.
fn find_clang() -> Option<String> {
    for name in CLANG_NAMES {
        let runs = Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if runs {
            return Some(name.to_string());
        }
    }
    None
}

/// The layout clang computes for a triple.
///
/// A C program is compiled to LLVM IR, and the IR carries the layout.
fn clang_layout(clang: &str, triple: &str) -> Option<String> {
    let mut child = Command::new(clang)
        .args([
            "-target",
            triple,
            "-S",
            "-emit-llvm",
            "-x",
            "c",
            "-",
            "-o",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // The program goes in on stdin; dropping the handle ends it, so clang
    // sees the end of the input.
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"int main(void) { return 0; }\n").ok()?;
    }
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }
    let ir = String::from_utf8_lossy(&out.stdout);
    ir.lines()
        .find_map(|line| line.strip_prefix("target datalayout = \""))
        .and_then(|rest| rest.strip_suffix('"'))
        .map(|layout| layout.to_string())
}

/// The layout as a map from spec name to spec value.
fn parse_layout(layout: &str) -> BTreeMap<String, String> {
    layout
        .split('-')
        .filter(|spec| !spec.is_empty())
        .map(|spec| {
            let (name, value) = spec.split_once(':').unwrap_or((spec, ""));
            (name.to_string(), value.to_string())
        })
        .collect()
}

/// Assert that our layout and clang's describe the same ABI.
///
/// Ours may be ahead of clang's by a spec upstream added after the
/// installed clang was released, but it may never be behind it: a spec
/// clang has and we do not is a layout that lays the data out
/// differently from the way clang lays it out.
fn assert_layouts_agree(target: &str, ours: &str, clang: &str) {
    let ours_specs = parse_layout(ours);
    let clang_specs = parse_layout(clang);
    for (name, value) in &clang_specs {
        match ours_specs.get(name) {
            None => panic!(
                "{target}: the data layout is missing '{name}', which clang's layout \
                 ({clang}) has"
            ),
            Some(ours_value) if ours_value != value => panic!(
                "{target}: the data layout says {name}:{ours_value}, clang's layout \
                 ({clang}) says {name}:{value}"
            ),
            Some(_) => {}
        }
    }
    for (name, value) in &ours_specs {
        if clang_specs.contains_key(name) {
            continue;
        }
        assert!(
            SPECS_ADDED_UPSTREAM.contains(&(name.as_str(), value.as_str())),
            "{target}: the data layout has '{name}:{value}', which clang's layout \
             ({clang}) does not have"
        );
    }
}
