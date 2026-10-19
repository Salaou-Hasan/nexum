//! `nx-bench` - the Nexum benchmark harness.
//!
//! The programme that owns the machine has to be able to say how fast it is.
//! This harness is how: every case is a Nexum program with a machine-readable
//! header, every case is compiled and checksummed before it is timed, and
//! every number is reported next to the machine it was measured on and the
//! flags the measured program was built with.
//!
//! # The case format
//!
//! The first comment line of a `.nx` file carries the header:
//!
//! ```text
//! # BENCH unit=ns-per-call ops=10000000 expect=50000005000000 ir_check=call i64 @noinline
//! ```
//!
//! * `unit` - the label of the thing being measured, shown in the report.
//! * `ops`  - the number of iterations of the operation under test. It must
//!   equal the iteration count in the source; the harness never rewrites the
//!   source, and the equality is caught by the checksum.
//! * `expect` - the value the program must print on its last line. This is
//!   what makes a benchmark a correctness test as well as a measurement: a
//!   case that measures the wrong thing fails instead of reporting a number.
//! * `ir_check` - a substring the emitted LLVM IR must contain. It is how a
//!   case proves it is still measuring the thing it claims to measure.
//! * `asm_check` - a function name; the compiled assembly must still call it.
//!   The IR check is not enough: clang inlines user functions, and a call
//!   benchmark whose callee was inlined measures an empty loop.
//! * `base` - a suite-relative path to the paired baseline: the same program
//!   with the operation under test removed. The harness subtracts the
//!   baseline's median from the case's median, which is what removes the
//!   process start-up and the loop bookkeeping from the result.
//! * `status=blocked` - the case is compiled and checksummed but not timed,
//!   and the report says why.
//! * `mir_opt` / `clang_opt` - the case is only valid at these levels; the
//!   harness refuses to run it at any other level.
//! * `note` - a quoted string with a human-readable explanation.
//!
//! # The statistics
//!
//! Fifteen timed runs per case, the median as the statistic, the paired
//! baseline subtracted, and the result divided by `ops`. The noise of the
//! machine is the relative interquartile range of the same fifteen samples,
//! and a regression is a change larger than `max(2%, 3 * noise)`. The machine
//! profile is part of the baseline: a number without the machine it was
//! measured on is not a result.

use clap::{Parser, Subcommand};
use nexum_diagnostics::Severity;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

// ---------------------------------------------------------------- the CLI

/// The suite directory: the repository's `benchmarks/suite`.
fn default_suite() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .join("..")
        .join("..")
        .join("benchmarks")
        .join("suite")
}

#[derive(Parser)]
#[command(name = "nx-bench", version, about = "The Nexum benchmark harness")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
    /// The suite directory (the one with `cases/` and `reference/`).
    #[arg(long, global = true)]
    suite: Option<PathBuf>,
    /// How many timed runs per case.
    #[arg(long, global = true, default_value_t = 15)]
    runs: usize,
    /// The MIR optimisation level of the measured program.
    #[arg(long, global = true, default_value_t = 1)]
    mir_opt: u8,
    /// The clang optimisation level of the measured program.
    #[arg(long, global = true, default_value_t = 3)]
    clang_opt: u8,
    /// Pin the measurement to this core.
    #[arg(long, global = true)]
    pin: Option<usize>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List the cases and their parsed headers.
    List {
        /// Only the cases whose path contains this text.
        pattern: Option<String>,
    },
    /// Compile every case, run it once, check the checksum.
    Smoke {
        /// Only the cases whose path contains this text.
        pattern: Option<String>,
    },
    /// Time the cases and print the table.
    Run {
        /// Only the cases whose path contains this text.
        pattern: Option<String>,
        /// Compare the result against the recorded baseline.
        #[arg(long)]
        baseline: bool,
    },
    /// Time the cases and write the baseline file.
    Record {
        /// Only the cases whose path contains this text.
        pattern: Option<String>,
    },
    /// The machine profile and the toolchain versions.
    Doctor,
}

impl Cli {
    /// The suite directory, defaulting to the repository's `benchmarks/suite`.
    fn suite(&self) -> PathBuf {
        self.suite.clone().unwrap_or_else(default_suite)
    }
}

fn main() {
    let cli = Cli::parse();
    let result = match &cli.cmd {
        Cmd::List { pattern } => cmd_list(&cli, pattern.as_deref()),
        Cmd::Smoke { pattern } => cmd_smoke(&cli, pattern.as_deref()),
        Cmd::Run { pattern, baseline } => cmd_run(&cli, pattern.as_deref(), *baseline),
        Cmd::Record { pattern } => cmd_record(&cli, pattern.as_deref()),
        Cmd::Doctor => cmd_doctor(&cli),
    };
    if let Err(e) = result {
        eprintln!("nx-bench: {e}");
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------- the case

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// The case is measured.
    Ready,
    /// The case is compiled and checksummed, but not timed.
    Blocked,
}

#[derive(Debug, Clone)]
struct Case {
    /// The path relative to the suite root, e.g. `cases/micro/call_value.nx`.
    rel: String,
    path: PathBuf,
    unit: String,
    ops: Option<u64>,
    expect: Option<String>,
    ir_check: Option<String>,
    asm_check: Option<String>,
    /// The paired baseline, relative to the suite root.
    base: Option<String>,
    note: Option<String>,
    status: Status,
    mir_opt: Option<u8>,
    clang_opt: Option<u8>,
}

impl Case {
    /// The file stem, which is also the name of the C twin.
    fn stem(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// The header fields.
#[derive(Debug, Default, Clone)]
struct Header {
    unit: Option<String>,
    ops: Option<u64>,
    expect: Option<String>,
    ir_check: Option<String>,
    asm_check: Option<String>,
    base: Option<String>,
    note: Option<String>,
    status: Option<String>,
    mir_opt: Option<u8>,
    clang_opt: Option<u8>,
}

/// Parse the `# BENCH` header of a case file.
fn parse_header(text: &str) -> Result<Header, String> {
    let mut fields: Option<Vec<(String, String)>> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !line.starts_with('#') {
            break;
        }
        let rest = line[1..].trim();
        if let Some(rest) = rest.strip_prefix("BENCH") {
            fields = Some(parse_fields(rest)?);
            break;
        }
    }
    let fields = fields.ok_or("no `# BENCH` header")?;
    let mut header = Header::default();
    for (key, value) in fields {
        match key.as_str() {
            "unit" => header.unit = Some(value),
            "ops" => {
                header.ops = Some(
                    value
                        .parse()
                        .map_err(|_| format!("ops must be an integer, got '{value}'"))?,
                )
            }
            "expect" => header.expect = Some(value),
            "ir_check" => header.ir_check = Some(value),
            "asm_check" => header.asm_check = Some(value),
            "base" => header.base = Some(value),
            "note" => header.note = Some(value),
            "status" => header.status = Some(value),
            "mir_opt" => {
                header.mir_opt = Some(
                    value
                        .parse()
                        .map_err(|_| format!("mir_opt must be an integer, got '{value}'"))?,
                )
            }
            "clang_opt" => {
                header.clang_opt = Some(
                    value
                        .parse()
                        .map_err(|_| format!("clang_opt must be an integer, got '{value}'"))?,
                )
            }
            other => return Err(format!("unknown header field '{other}'")),
        }
    }
    Ok(header)
}

/// Split `key=value key=value` into pairs.
///
/// A value may contain spaces (`ir_check=call i64 @noinline` does), so an
/// unquoted value ends at the next `word=` and not at the next space. A value
/// that contains a space *and* something that looks like `word=` has to be
/// quoted.
fn parse_fields(s: &str) -> Result<Vec<(String, String)>, String> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[start..i].iter().collect();
        if i >= chars.len() || chars[i] != '=' {
            return Err(format!("header field '{key}' has no value"));
        }
        i += 1;
        let value: String;
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            let vs = i;
            while i < chars.len() && chars[i] != '"' {
                i += 1;
            }
            if i >= chars.len() {
                return Err(format!("unterminated quoted value for '{key}'"));
            }
            value = chars[vs..i].iter().collect();
            i += 1;
        } else {
            let vs = i;
            loop {
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
                if i >= chars.len() {
                    break;
                }
                // Look ahead: the value ends if the next word is a field name.
                let mut j = i;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                let mut k = j;
                if k < chars.len() && (chars[k].is_alphabetic() || chars[k] == '_') {
                    while k < chars.len() && (chars[k].is_alphanumeric() || chars[k] == '_') {
                        k += 1;
                    }
                    if k < chars.len() && chars[k] == '=' {
                        break;
                    }
                }
                i = j;
            }
            let raw: String = chars[vs..i].iter().collect();
            value = raw.trim().to_string();
        }
        out.push((key, value));
    }
    Ok(out)
}

/// Find every case file under the suite's `cases/` directory.
fn discover(suite: &Path) -> Result<Vec<Case>, String> {
    let root = suite.join("cases");
    let mut files = Vec::new();
    collect_nx(&root, &mut files)?;
    files.sort();
    let mut cases = Vec::new();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let header = parse_header(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        let unit = header
            .unit
            .clone()
            .ok_or_else(|| format!("{}: the header has no 'unit'", path.display()))?;
        let status = match header.status.as_deref() {
            None | Some("ready") => Status::Ready,
            Some("blocked") => Status::Blocked,
            Some(other) => {
                return Err(format!(
                    "{}: unknown status '{other}' (use 'ready' or 'blocked')",
                    path.display()
                ));
            }
        };
        let rel = path
            .strip_prefix(suite)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        cases.push(Case {
            rel,
            path,
            unit,
            ops: header.ops,
            expect: header.expect,
            ir_check: header.ir_check,
            asm_check: header.asm_check,
            base: header.base,
            note: header.note,
            status,
            mir_opt: header.mir_opt,
            clang_opt: header.clang_opt,
        });
    }
    Ok(cases)
}

fn collect_nx(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if !dir.exists() {
        return Err(format!("{}: no such directory", dir.display()));
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .map(|e| {
            e.map(|e| e.path())
                .map_err(|e| format!("{}: {e}", dir.display()))
        })
        .collect::<Result<_, _>>()?;
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_nx(&path, out)?;
        } else if path.extension().map(|e| e == "nx").unwrap_or(false) {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- building

/// The flags the measured program is built with.
#[derive(Debug, Clone)]
struct Flags {
    mir_opt: u8,
    clang_opt: u8,
    lto: bool,
}

impl Flags {
    /// The clang command line, for the report and the baseline file.
    fn clang_line(&self) -> String {
        let mut s = format!("clang -O{}", self.clang_opt);
        if self.lto {
            s.push_str(" -flto=thin");
        }
        s
    }
}

/// A built program: the executable and, for a Nexum program, its IR.
struct Built {
    exe: PathBuf,
    ir: Option<String>,
}

/// Compile a Nexum program: front end, MIR, LLVM IR, then clang.
fn build_nx(case: &Case, flags: &Flags, work: &Path, stem: &str) -> Result<Built, String> {
    let mut sm = nexum_source::SourceMap::new();
    let (module, load_diags) = nexum_parser::load_module(&mut sm, &case.path);
    if let Some(d) = first_error(&load_diags) {
        return Err(format!("{}: {d}", case.rel));
    }
    let sema = nexum_sema::analyze(&module);
    if let Some(d) = first_error(&sema.diagnostics) {
        return Err(format!("{}: {d}", case.rel));
    }
    let hir = sema.hir.ok_or_else(|| format!("{}: no hir", case.rel))?;
    let lowered = nexum_mir::lower(&hir);
    if let Some(d) = first_error(&lowered.diagnostics) {
        return Err(format!("{}: {d}", case.rel));
    }
    let mut mir = lowered.module;
    nexum_mir::optimize_level(&mut mir, flags.mir_opt);
    if let Some(d) = first_error(&nexum_mir::verify(&mir)) {
        return Err(format!("{}: {d}", case.rel));
    }
    let target = nexum_target::TargetSpec::host();
    let ir = nexum_codegen::emit(&mir, &target);
    let ll = work.join(format!("{stem}.ll"));
    fs::write(&ll, &ir).map_err(|e| format!("{}: {e}", ll.display()))?;
    let exe = work.join(format!("{stem}{}", exe_suffix()));
    let tc = nexum_toolchain::Toolchain::discover()?;
    let opts = nexum_toolchain::BuildOpts {
        opt: flags.clang_opt,
        lto: flags.lto,
        sysroot: None,
        pgo_generate: false,
        pgo_use: None,
        target: Some(target.triple.clone()),
    };
    nexum_toolchain::run_clang_with(
        &tc.clang,
        &ll.to_string_lossy(),
        &exe.to_string_lossy(),
        &opts,
    )?;
    Ok(Built { exe, ir: Some(ir) })
}

/// Compile the C twin: the same algorithm, the same clang, the same flags.
fn build_c(twin: &Path, flags: &Flags, work: &Path, stem: &str) -> Result<PathBuf, String> {
    let exe = work.join(format!("{stem}{}", exe_suffix()));
    let tc = nexum_toolchain::Toolchain::discover()?;
    let status = Command::new(&tc.clang)
        .arg(format!("-O{}", flags.clang_opt))
        // The twins are plain C and the C library's own headers are noisy
        // on Windows; a warning is not a measurement.
        .arg("-Wno-deprecated-declarations")
        .arg(twin)
        .arg("-o")
        .arg(&exe)
        .status()
        .map_err(|e| format!("{}: {e}", twin.display()))?;
    if !status.success() {
        return Err(format!("{}: clang failed with {status}", twin.display()));
    }
    Ok(exe)
}

/// Compile the IR to assembly, so a case can prove that a call survived.
fn build_asm(clang: &str, ll: &Path, flags: &Flags, out: &Path) -> Result<String, String> {
    let status = Command::new(clang)
        .arg("-Wno-override-module")
        .arg(format!("-O{}", flags.clang_opt))
        .arg("-S")
        .arg("-o")
        .arg(out)
        .arg(ll)
        .status()
        .map_err(|e| format!("{}: {e}", ll.display()))?;
    if !status.success() {
        return Err(format!("{}: clang failed with {status}", ll.display()));
    }
    fs::read_to_string(out).map_err(|e| format!("{}: {e}", out.display()))
}

/// Does the assembly still call (or tail-call) the given symbol?
fn assembly_calls(asm: &str, symbol: &str) -> bool {
    asm.lines().any(|line| {
        let line = line.trim();
        if !(line.starts_with("call") || line.starts_with("jmp")) {
            return false;
        }
        line.split_whitespace().any(|token| {
            let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '_');
            token == symbol || token == format!("_{symbol}")
        })
    })
}

fn first_error(diags: &[nexum_diagnostics::Diagnostic]) -> Option<String> {
    diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code.0, d.message))
        .next()
}

fn exe_suffix() -> &'static str {
    if cfg!(windows) { ".exe" } else { "" }
}

// ---------------------------------------------------------------- running

/// Run a program and capture its stdout.
fn capture(exe: &Path, args: &[&str], work: &Path) -> Result<String, String> {
    let out = Command::new(exe)
        .args(args)
        .current_dir(work)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    if !out.status.success() {
        return Err(format!(
            "{}: exited with {}",
            exe.display(),
            out.status.code().unwrap_or(-1)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The last non-empty line of the output: the case's checksum.
fn checksum(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .map(|l| l.trim())
        .rfind(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// Compare the printed checksum with the header's `expect`.
fn expect_matches(got: &str, want: &str) -> bool {
    if let (Ok(a), Ok(b)) = (got.parse::<i64>(), want.parse::<i64>()) {
        return a == b;
    }
    match (got.parse::<f64>(), want.parse::<f64>()) {
        (Ok(a), Ok(b)) => {
            if a == b {
                return true;
            }
            let scale = a.abs().max(b.abs());
            scale > 0.0 && (a - b).abs() / scale <= 1e-9
        }
        _ => got.trim() == want.trim(),
    }
}

/// The time one run of a program took.
struct RunOnce {
    elapsed_ms: f64,
}

fn run_once(exe: &Path, args: &[&str], work: &Path) -> Result<RunOnce, String> {
    let start = Instant::now();
    let out = Command::new(exe)
        .args(args)
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let elapsed = start.elapsed().as_secs_f64() * 1000.0;
    if !out.success() {
        return Err(format!(
            "{}: exited with {}",
            exe.display(),
            out.code().unwrap_or(-1)
        ));
    }
    Ok(RunOnce {
        elapsed_ms: elapsed,
    })
}

/// Time a program: one warm-up run that is thrown away, then `runs` samples.
///
/// The warm-up exists for the page cache and for the CPU: the first seconds
/// of a run are often faster than the rest, and that is exactly the sample
/// that would be wrong.
fn time_program(exe: &Path, args: &[&str], runs: usize, work: &Path) -> Result<Vec<f64>, String> {
    let _ = run_once(exe, args, work)?;
    let mut samples = Vec::with_capacity(runs);
    for _ in 0..runs {
        samples.push(run_once(exe, args, work)?.elapsed_ms);
    }
    Ok(samples)
}

// ---------------------------------------------------------------- statistics

/// The summary of a set of samples.
#[derive(Debug, Clone, Copy)]
struct Stats {
    min: f64,
    p25: f64,
    median: f64,
    p75: f64,
    max: f64,
    /// The relative interquartile range: how much this machine can be
    /// trusted today.
    noise: f64,
}

impl Stats {
    fn of(samples: &[f64]) -> Stats {
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = sorted.len();
        if n == 0 {
            return Stats {
                min: 0.0,
                p25: 0.0,
                median: 0.0,
                p75: 0.0,
                max: 0.0,
                noise: 0.0,
            };
        }
        let pick = |p: f64| -> f64 {
            let rank = ((p * n as f64).ceil() as usize).saturating_sub(1);
            sorted[rank.min(n - 1)]
        };
        let min = sorted.first().copied().unwrap_or(0.0);
        let max = sorted.last().copied().unwrap_or(0.0);
        let median = if n % 2 == 1 {
            sorted[n / 2]
        } else {
            (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
        };
        let p25 = pick(0.25);
        let p75 = pick(0.75);
        let noise = if median > 0.0 {
            (p75 - p25) / median
        } else {
            0.0
        };
        Stats {
            min,
            p25,
            median,
            p75,
            max,
            noise,
        }
    }
}

// ---------------------------------------------------------------- the machine

/// The machine the numbers were measured on. A baseline recorded on a
/// different profile is not comparable, so the profile is part of it.
#[derive(Debug, Clone)]
struct Machine {
    cpu: String,
    cores: usize,
    os: String,
    power: String,
    clang: String,
    rustc: String,
    pinned: String,
    /// A hash of the parts that decide whether two runs are comparable.
    profile: String,
}

impl Machine {
    fn detect(pin: Option<usize>) -> Machine {
        let cpu = cpu_model();
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0);
        let os = os_version();
        let power = power_source();
        let clang = tool_version("clang");
        let rustc = tool_version("rustc");
        let pinned = match pin {
            Some(core) => match pin_to_core(core) {
                Ok(()) => format!("core {core}"),
                Err(e) => format!("requested core {core}, not applied: {e}"),
            },
            None => "not pinned".to_string(),
        };
        let profile = profile_id(&cpu, cores, &os, &power);
        Machine {
            cpu,
            cores,
            os,
            power,
            clang,
            rustc,
            pinned,
            profile,
        }
    }

    /// One line for the report.
    fn line(&self) -> String {
        format!(
            "{} - {} logical cores - {} - power {}",
            self.cpu, self.cores, self.os, self.power
        )
    }
}

/// FNV-1a over the parts of the machine that decide comparability.
fn profile_id(cpu: &str, cores: usize, os: &str, power: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in cpu
        .bytes()
        .chain(cores.to_string().bytes())
        .chain(os.bytes())
        .chain(power.bytes())
    {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

fn cpu_model() -> String {
    if cfg!(windows) {
        return reg_value(
            r"HKEY_LOCAL_MACHINE\HARDWARE\DESCRIPTION\System\CentralProcessor\0",
            "ProcessorNameString",
        )
        .unwrap_or_else(|| "unknown cpu".to_string());
    }
    if cfg!(target_os = "macos") {
        return command_output("sysctl", &["-n", "machdep.cpu.brand_string"])
            .unwrap_or_else(|| "unknown cpu".to_string());
    }
    if let Ok(text) = fs::read_to_string("/proc/cpuinfo") {
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("model name")
                && let Some((_, value)) = rest.split_once(':')
            {
                return value.trim().to_string();
            }
        }
    }
    "unknown cpu".to_string()
}

fn os_version() -> String {
    if cfg!(windows) {
        let name = reg_value(
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "ProductName",
        )
        .unwrap_or_else(|| "windows".to_string());
        let build = reg_value(
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "CurrentBuildNumber",
        )
        .unwrap_or_default();
        if build.is_empty() {
            name
        } else {
            format!("{name} {build}")
        }
    } else if cfg!(target_os = "macos") {
        command_output("sw_vers", &["-productVersion"]).unwrap_or_else(|| "macos".to_string())
    } else {
        if let Ok(text) = fs::read_to_string("/etc/os-release") {
            for line in text.lines() {
                if let Some(rest) = line.strip_prefix("PRETTY_NAME=") {
                    return rest.trim_matches('"').to_string();
                }
            }
        }
        std::env::consts::OS.to_string()
    }
}

/// Is the machine on mains power, and which power plan is active?
fn power_source() -> String {
    if cfg!(windows) {
        let scheme = command_output("powercfg", &["/GETACTIVESCHEME"])
            .and_then(|s| {
                // The plan is a GUID; the line is localised, so the GUID is
                // found by its shape and not by the words around it.
                s.split_whitespace()
                    .map(|token| token.trim_matches(|c: char| !c.is_ascii_hexdigit() && c != '-'))
                    .find(|token| {
                        let parts: Vec<&str> = token.split('-').collect();
                        parts.len() == 5
                            && [8usize, 4, 4, 4, 12]
                                .iter()
                                .zip(parts.iter())
                                .all(|(len, part)| part.len() == *len)
                    })
                    .map(|token| token.to_string())
            })
            .unwrap_or_else(|| "unknown".to_string());
        let ac = command_output(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance -ClassName Win32_Battery).BatteryStatus",
            ],
        )
        .map(|s| {
            let s = s.trim();
            if s.is_empty() || s == "2" {
                "ac".to_string()
            } else if s == "1" {
                "battery".to_string()
            } else {
                format!("battery status {s}")
            }
        })
        .unwrap_or_else(|| "unknown".to_string());
        format!("{ac}, plan {scheme}")
    } else if cfg!(target_os = "macos") {
        let out = command_output("pmset", &["-g", "ps"]).unwrap_or_default();
        if out.contains("AC Power") {
            "ac".to_string()
        } else {
            "battery".to_string()
        }
    } else {
        let mut ac = None;
        if let Ok(entries) = fs::read_dir("/sys/class/power_supply") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("AC")
                    && let Ok(text) = fs::read_to_string(entry.path().join("online"))
                {
                    ac = Some(text.trim() == "1");
                }
            }
        }
        let governor = fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|_| "unknown governor".to_string());
        match ac {
            Some(true) => format!("ac, governor {governor}"),
            Some(false) => format!("battery, governor {governor}"),
            None => format!("mains, governor {governor}"),
        }
    }
}

fn tool_version(tool: &str) -> String {
    command_output(tool, &["--version"])
        .map(|s| s.lines().next().unwrap_or("").trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Read one value out of the Windows registry.
fn reg_value(key: &str, name: &str) -> Option<String> {
    let out = command_output("reg", &["query", key, "/v", name])?;
    for line in out.lines() {
        if line.contains(name) {
            // The layout is `name<spaces>TYPE<spaces>data`; the data is the
            // last field, and the field separator is a run of spaces.
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() >= 3 {
                // The value may contain spaces, so take everything after the
                // type field.
                let after = line
                    .split_whitespace()
                    .skip(2)
                    .collect::<Vec<_>>()
                    .join(" ");
                if !after.is_empty() {
                    return Some(after);
                }
            }
        }
    }
    None
}

/// Pin the measurement to one core, so the run does not migrate between
/// cores with different cache and turbo behaviour.
fn pin_to_core(core: usize) -> Result<(), String> {
    #[cfg(unix)]
    {
        if command_output("taskset", &["-c", &core.to_string(), "true"]).is_none() {
            return Err("taskset is not available".to_string());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let mask: i64 = 1i64 << core;
        let script = format!(
            "$p = Get-Process -Id {}; $p.ProcessorAffinity = [IntPtr]{}",
            std::process::id(),
            mask
        );
        let ok = command_output("powershell", &["-NoProfile", "-Command", &script]).is_some();
        if ok {
            Ok(())
        } else {
            Err("could not set the processor affinity".to_string())
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = core;
        Err("this platform has no pinning support".to_string())
    }
}

// ---------------------------------------------------------------- the results

/// What the harness measured for one case.
struct CaseResult {
    case: Case,
    samples_ms: Vec<f64>,
    stats: Stats,
    /// The cost of one operation, with the paired baseline subtracted.
    ns_per_op: Option<f64>,
    /// The same, for the C twin.
    c_ns_per_op: Option<f64>,
    /// The comparison that was used, recorded so a reader knows what the
    /// number was compared against.
    comparison: Option<String>,
    /// Why the case was not timed.
    blocked: Option<String>,
}

/// The verdict against the recorded baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Improvement,
    Regression,
    Unchanged,
}

/// The machine and the flags, as they go into the baseline file.
struct Context {
    suite: PathBuf,
    work: PathBuf,
    flags: Flags,
    machine: Machine,
    /// How many timed runs per case.
    runs: usize,
}

impl Context {
    fn new(cli: &Cli) -> Result<Context, String> {
        let work = std::env::temp_dir().join("nx-bench");
        fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        Ok(Context {
            suite: cli.suite(),
            work,
            flags: Flags {
                mir_opt: cli.mir_opt,
                clang_opt: cli.clang_opt,
                lto: false,
            },
            machine: Machine::detect(cli.pin),
            runs: cli.runs,
        })
    }
}

// ---------------------------------------------------------------- the commands

fn cmd_list(cli: &Cli, pattern: Option<&str>) -> Result<(), String> {
    let cases = discover(&cli.suite())?;
    println!("{:<44} {:<16} {:>12}  status", "case", "unit", "ops");
    println!("{}", "-".repeat(96));
    for case in &cases {
        if !matches(case, pattern) {
            continue;
        }
        let status = match case.status {
            Status::Ready => "ready".to_string(),
            Status::Blocked => "blocked".to_string(),
        };
        let ops = case
            .ops
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        println!("{:<44} {:<16} {:>12}  {}", case.rel, case.unit, ops, status);
        if let Some(note) = &case.note {
            println!("{:<44} {}", "", note);
        }
    }
    Ok(())
}

fn cmd_smoke(cli: &Cli, pattern: Option<&str>) -> Result<(), String> {
    let ctx = Context::new(cli)?;
    let cases = discover(&ctx.suite)?;
    let mut failures = 0usize;
    let mut checked = 0usize;
    for case in &cases {
        if !matches(case, pattern) {
            continue;
        }
        checked += 1;
        match smoke_case(&ctx, case) {
            Ok(()) => println!("ok    {}", case.rel),
            Err(e) => {
                failures += 1;
                println!("FAIL  {e}");
            }
        }
    }
    println!("\n{checked} case(s) checked, {failures} failure(s).");
    if failures > 0 {
        return Err("the suite did not pass the smoke check".to_string());
    }
    Ok(())
}

/// Compile a case, check its checksum, and check that it still measures what
/// it claims to measure.
fn smoke_case(ctx: &Context, case: &Case) -> Result<(), String> {
    let flags = flags_for(ctx, case)?;
    let built = build_nx(case, &flags, &ctx.work, &case.stem())?;
    check_case(ctx, case, &built, &flags)?;
    let stdout = capture(&built.exe, &[], &ctx.work)?;
    let got =
        checksum(&stdout).ok_or_else(|| format!("{}: the program printed nothing", case.rel))?;
    if let Some(want) = &case.expect
        && !expect_matches(&got, want)
    {
        return Err(format!("{}: printed '{got}', expected '{want}'", case.rel));
    }
    Ok(())
}

/// The checks that make a case a measurement of what it claims to measure.
fn check_case(ctx: &Context, case: &Case, built: &Built, flags: &Flags) -> Result<(), String> {
    if let Some(check) = &case.ir_check {
        let ir = built.ir.as_deref().unwrap_or_default();
        if !ir.contains(check.as_str()) {
            return Err(format!(
                "{}: the emitted IR does not contain '{check}'",
                case.rel
            ));
        }
    }
    if let Some(symbol) = &case.asm_check {
        let ll = ctx.work.join(format!("{}.ll", case.stem()));
        let asm_path = ctx.work.join(format!("{}.s", case.stem()));
        let tc = nexum_toolchain::Toolchain::discover()?;
        let asm = build_asm(&tc.clang, &ll, flags, &asm_path)?;
        if !assembly_calls(&asm, symbol) {
            return Err(format!(
                "{}: the compiled program does not call '{symbol}' - the \
                 measurement would be of an inlined loop, not of a call",
                case.rel
            ));
        }
    }
    Ok(())
}

fn cmd_run(cli: &Cli, pattern: Option<&str>, compare: bool) -> Result<(), String> {
    let ctx = Context::new(cli)?;
    let (results, blocked) = measure(&ctx, pattern)?;
    print_report(&ctx, &results, &blocked);
    if compare {
        print_verdicts(&ctx, &results)?;
    }
    write_results(&ctx, &results, &blocked)?;
    Ok(())
}

fn cmd_record(cli: &Cli, pattern: Option<&str>) -> Result<(), String> {
    let ctx = Context::new(cli)?;
    let (results, blocked) = measure(&ctx, pattern)?;
    print_report(&ctx, &results, &blocked);
    let path = ctx.suite.join("baseline.json");
    let text = baseline_json(&ctx, &results);
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("\nrecorded: {}", path.display());
    Ok(())
}

fn cmd_doctor(cli: &Cli) -> Result<(), String> {
    let ctx = Context::new(cli)?;
    let m = &ctx.machine;
    println!("machine");
    println!("  cpu        {}", m.cpu);
    println!("  cores      {} logical", m.cores);
    println!("  os         {}", m.os);
    println!("  power      {}", m.power);
    println!("  pinned     {}", m.pinned);
    println!("  profile    {}", m.profile);
    println!("toolchain");
    println!("  clang      {}", m.clang);
    println!("  rustc      {}", m.rustc);
    let tc = nexum_toolchain::Toolchain::discover()?;
    println!("  clang path {}", tc.clang);
    println!("flags");
    println!("  mir_opt    {}", ctx.flags.mir_opt);
    println!("  clang_opt  {}", ctx.flags.clang_opt);
    println!("  lto        {}", ctx.flags.lto);
    println!("  runs       {}", ctx.runs);
    println!("suite");
    println!("  path       {}", ctx.suite.display());
    Ok(())
}

/// The flags a case is measured with: the harness's, unless the case pins
/// its own. A case that is only valid at one level says so, and the harness
/// refuses to run it at any other level.
fn flags_for(ctx: &Context, case: &Case) -> Result<Flags, String> {
    let mut flags = ctx.flags.clone();
    if let Some(level) = case.mir_opt {
        if level != flags.mir_opt {
            return Err(format!(
                "{}: this case is only valid at MIR level {level}, and the \
                 harness is running at level {}",
                case.rel, flags.mir_opt
            ));
        }
        flags.mir_opt = level;
    }
    if let Some(level) = case.clang_opt {
        if level != flags.clang_opt {
            return Err(format!(
                "{}: this case is only valid at clang -O{level}, and the \
                 harness is running at -O{}",
                case.rel, flags.clang_opt
            ));
        }
        flags.clang_opt = level;
    }
    Ok(flags)
}

/// Measure every selected case, and the baselines they are paired with.
fn measure(
    ctx: &Context,
    pattern: Option<&str>,
) -> Result<(Vec<CaseResult>, Vec<CaseResult>), String> {
    let cases = discover(&ctx.suite)?;
    // A case that is somebody's paired baseline is only measured as part of
    // that pair; measuring it on its own would time a program that does
    // nothing interesting.
    let bases: Vec<&str> = cases.iter().filter_map(|c| c.base.as_deref()).collect();
    let selected: Vec<&Case> = cases
        .iter()
        .filter(|c| {
            matches(c, pattern) && c.status == Status::Ready && !bases.contains(&c.rel.as_str())
        })
        .collect();

    // A case is measured together with the baseline it is paired with.
    let mut work: Vec<&Case> = Vec::new();
    for case in &selected {
        if !work.iter().any(|c| c.rel == case.rel) {
            work.push(case);
        }
        if let Some(base) = &case.base
            && let Some(base_case) = cases.iter().find(|c| &c.rel == base)
            && !work.iter().any(|c| c.rel == base_case.rel)
        {
            work.push(base_case);
        }
    }

    let mut results: Vec<CaseResult> = Vec::new();
    let mut blocked: Vec<CaseResult> = Vec::new();
    let mut medians: BTreeMap<String, f64> = BTreeMap::new();

    for case in &work {
        let flags = flags_for(ctx, case)?;
        let built = build_nx(case, &flags, &ctx.work, &case.stem())?;
        // The correctness check comes before the measurement: a case that
        // computes the wrong number must not produce a timing.
        check_case(ctx, case, &built, &flags)?;
        let stdout = capture(&built.exe, &[], &ctx.work)?;
        let got = checksum(&stdout)
            .ok_or_else(|| format!("{}: the program printed nothing", case.rel))?;
        if let Some(want) = &case.expect
            && !expect_matches(&got, want)
        {
            return Err(format!("{}: printed '{got}', expected '{want}'", case.rel));
        }
        let samples = time_program(&built.exe, &[], ctx.runs, &ctx.work)?;
        let stats = Stats::of(&samples);
        medians.insert(case.rel.clone(), stats.median);
        results.push(CaseResult {
            case: (*case).clone(),
            samples_ms: samples,
            stats,
            ns_per_op: None,
            c_ns_per_op: None,
            comparison: None,
            blocked: None,
        });
    }

    // The blocked cases are compiled and checksummed, but not timed.
    for case in &cases {
        if !matches(case, pattern) || case.status != Status::Blocked {
            continue;
        }
        let reason = case
            .note
            .clone()
            .unwrap_or_else(|| "blocked by the case header".to_string());
        let twin_note = match check_twin_builds(ctx, case) {
            Ok(()) => "the C twin is in place and builds".to_string(),
            Err(e) => format!("the C twin does not build: {e}"),
        };
        let blocked_note = match smoke_case(ctx, case) {
            Ok(()) => reason,
            Err(e) => format!("{reason} (and the case does not build: {e})"),
        };
        blocked.push(CaseResult {
            case: case.clone(),
            samples_ms: Vec::new(),
            stats: Stats::of(&[]),
            ns_per_op: None,
            c_ns_per_op: None,
            comparison: Some(twin_note),
            blocked: Some(blocked_note),
        });
    }

    // Subtract the paired baseline, and measure the C twin.
    for result in &mut results {
        let case = &result.case;
        let Some(base) = &case.base else {
            continue;
        };
        let Some(base_median) = medians.get(base) else {
            continue;
        };
        let Some(ops) = case.ops else {
            continue;
        };
        if *base_median > result.stats.median {
            // The case cannot be slower than its own baseline; that means the
            // baseline is not the baseline of this case, or the machine was
            // too noisy for the difference to mean anything.
            eprintln!(
                "warning: {} is faster than its baseline '{base}' - the \
                 difference is inside the noise, or the baseline does not \
                 match the case",
                case.rel
            );
            result.ns_per_op = Some((result.stats.median - base_median) * 1e6 / ops as f64);
            continue;
        }
        result.ns_per_op = Some((result.stats.median - base_median) * 1e6 / ops as f64);
        if let Some((c_ns, comparison)) = measure_twin(ctx, case, ops)? {
            result.c_ns_per_op = Some(c_ns);
            result.comparison = Some(comparison);
        }
    }

    Ok((results, blocked))
}

/// Check that the C twin of a case still builds, so a blocked case does not
/// rot while it waits for the language to catch up.
fn check_twin_builds(ctx: &Context, case: &Case) -> Result<(), String> {
    let twin_rel = format!("reference/{}.c", case.stem());
    let twin = ctx.suite.join(&twin_rel);
    if !twin.exists() {
        return Err(format!("{} is missing", twin_rel));
    }
    let flags = flags_for(ctx, case)?;
    build_c(&twin, &flags, &ctx.work, &format!("{}-c", case.stem()))?;
    Ok(())
}

/// Measure the C twin: the same algorithm, the same clang, the same flags.
///
/// The twin is run twice, in the `case` mode and in the `base` mode, and the
/// difference between the two is the cost of the operation in C. The Nexum
/// side is measured the same way, so the ratio compares the same thing.
fn measure_twin(ctx: &Context, case: &Case, ops: u64) -> Result<Option<(f64, String)>, String> {
    let twin_rel = format!("reference/{}.c", case.stem());
    let twin = ctx.suite.join(&twin_rel);
    if !twin.exists() {
        return Ok(None);
    }
    let flags = flags_for(ctx, case)?;
    let exe = build_c(&twin, &flags, &ctx.work, &format!("{}-c", case.stem()))?;
    let ops_str = ops.to_string();
    // The twin must compute the same thing as the Nexum case: the checksum
    // is what makes the comparison a comparison of the same algorithm.
    let twin_out = capture(&exe, &["case", &ops_str], &ctx.work)?;
    let got =
        checksum(&twin_out).ok_or_else(|| format!("{}: the C twin printed nothing", twin_rel))?;
    if let Some(want) = &case.expect
        && !expect_matches(&got, want)
    {
        return Err(format!(
            "{}: the C twin printed '{got}', the Nexum case expects \
             '{want}' - they are not the same algorithm",
            twin_rel
        ));
    }
    let case_samples = time_program(&exe, &["case", &ops_str], ctx.runs, &ctx.work)?;
    let case_stats = Stats::of(&case_samples);
    let mut comparison = format!("{} ({})", twin_rel, flags.clang_line());
    if case.base.is_some() {
        let base_out = capture(&exe, &["base", &ops_str], &ctx.work)?;
        let base_got = checksum(&base_out)
            .ok_or_else(|| format!("{}: the C twin printed nothing", twin_rel))?;
        // The base program has a header of its own, with its own checksum.
        let base_case = discover(&ctx.suite)?
            .into_iter()
            .find(|c| Some(&c.rel) == case.base.as_ref());
        if let Some(want) = base_case.and_then(|c| c.expect.clone())
            && !expect_matches(&base_got, &want)
        {
            return Err(format!(
                "{}: the C twin's base mode printed '{base_got}', the base \
                 case expects '{want}'",
                twin_rel
            ));
        }
        let base_samples = time_program(&exe, &["base", &ops_str], ctx.runs, &ctx.work)?;
        let base_stats = Stats::of(&base_samples);
        if base_stats.median > case_stats.median {
            // The case mode cannot be slower than the base mode; that means
            // the two modes do not do the same work.
            eprintln!(
                "warning: {}: the C twin's case mode is faster than its base \
                 mode - the two modes do not do the same work",
                twin_rel
            );
        }
        let c_ns = (case_stats.median - base_stats.median) * 1e6 / ops as f64;
        comparison.push_str(", modes case|base");
        Ok(Some((c_ns, comparison)))
    } else {
        let c_ns = case_stats.median * 1e6 / ops as f64;
        comparison.push_str(", mode case");
        Ok(Some((c_ns, comparison)))
    }
}

// ---------------------------------------------------------------- the report

/// Print the table.
fn print_report(ctx: &Context, results: &[CaseResult], blocked: &[CaseResult]) {
    let m = &ctx.machine;
    println!("Nexum benchmark suite");
    println!("  suite      {}", ctx.suite.display());
    println!("  machine    {}", m.line());
    println!("  profile    {}", m.profile);
    println!("  pinned     {}", m.pinned);
    println!("  toolchain  {}, {}", m.clang, m.rustc);
    println!(
        "  flags      mir_opt={} clang_opt={} lto={} runs={}",
        ctx.flags.mir_opt, ctx.flags.clang_opt, ctx.flags.lto, ctx.runs
    );
    println!();

    let measured: Vec<&CaseResult> = results.iter().filter(|r| r.ns_per_op.is_some()).collect();
    let baselines: Vec<&CaseResult> = results.iter().filter(|r| r.ns_per_op.is_none()).collect();

    if !measured.is_empty() {
        println!("measured (the paired baseline is subtracted)");
        let headers = [
            "case", "unit", "ops", "ns/op", "median", "min", "p25", "p75", "max", "noise",
        ];
        let mut rows = Vec::new();
        for r in &measured {
            rows.push(vec![
                r.case.rel.clone(),
                r.case.unit.clone(),
                r.case.ops.map(|n| n.to_string()).unwrap_or_default(),
                format!("{:.2}", r.ns_per_op.unwrap()),
                format!("{:.1}", r.stats.median),
                format!("{:.1}", r.stats.min),
                format!("{:.1}", r.stats.p25),
                format!("{:.1}", r.stats.p75),
                format!("{:.1}", r.stats.max),
                format!("{:.3}", r.stats.noise),
            ]);
        }
        print_table(&headers, &rows, &[2, 3, 4, 5, 6, 7, 8, 9]);
        println!();
    }

    if !baselines.is_empty() {
        println!("baselines (the paired programs the result is subtracted from)");
        let headers = ["case", "unit", "ops", "median", "min", "max", "noise"];
        let mut rows = Vec::new();
        for r in &baselines {
            rows.push(vec![
                r.case.rel.clone(),
                r.case.unit.clone(),
                r.case.ops.map(|n| n.to_string()).unwrap_or_default(),
                format!("{:.1}", r.stats.median),
                format!("{:.1}", r.stats.min),
                format!("{:.1}", r.stats.max),
                format!("{:.3}", r.stats.noise),
            ]);
        }
        print_table(&headers, &rows, &[2, 3, 4, 5, 6]);
        println!();
    }

    let with_twin: Vec<&CaseResult> = results
        .iter()
        .filter(|r| r.c_ns_per_op.is_some() && r.ns_per_op.is_some())
        .collect();
    if !with_twin.is_empty() {
        println!("the C reference (the same algorithm, the same clang, the same flags)");
        let headers = [
            "case",
            "unit",
            "c ns/op",
            "nexum ns/op",
            "ratio",
            "comparison",
        ];
        let mut rows = Vec::new();
        for r in &with_twin {
            let c = r.c_ns_per_op.unwrap();
            let nx = r.ns_per_op.unwrap();
            let ratio = if c > 0.0 { nx / c } else { f64::NAN };
            rows.push(vec![
                r.case.rel.clone(),
                r.case.unit.clone(),
                format!("{c:.2}"),
                format!("{nx:.2}"),
                if ratio.is_finite() {
                    format!("{ratio:.2}x")
                } else {
                    "-".to_string()
                },
                r.comparison.clone().unwrap_or_default(),
            ]);
        }
        print_table(&headers, &rows, &[2, 3, 4]);
        println!();
    }

    if !blocked.is_empty() {
        println!("not yet measurable");
        for r in blocked {
            println!(
                "  {}: {}",
                r.case.rel,
                r.blocked.clone().unwrap_or_default()
            );
            if let Some(note) = &r.comparison {
                println!("      {note}");
            }
        }
        println!();
    }

    for r in &measured {
        if r.stats.median < 100.0 {
            println!(
                "warning: {} finished in {:.1} ms - the case is too short to \
                 trust, raise its ops",
                r.case.rel, r.stats.median
            );
        } else if r.stats.median > 30_000.0 {
            println!(
                "warning: {} took {:.1} s - the case is longer than it needs \
                 to be",
                r.case.rel,
                r.stats.median / 1000.0
            );
        }
    }
}

/// Print a table with the columns aligned on their widest cell. The columns
/// whose index is in `right` are right-aligned; the rest are left-aligned.
fn print_table(headers: &[&str], rows: &[Vec<String>], right: &[usize]) {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.len());
            }
        }
    }
    let line = |cells: &[String]| -> String {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i > 0 {
                out.push_str("  ");
            }
            if right.contains(&i) {
                out.push_str(&format!("{cell:>width$}", width = widths[i]));
            } else {
                out.push_str(&format!("{cell:<width$}", width = widths[i]));
            }
        }
        out
    };
    let header_cells: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
    println!("{}", line(&header_cells));
    let mut rule = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            rule.push_str("  ");
        }
        rule.push_str(&"-".repeat(*w));
    }
    println!("{rule}");
    for row in rows {
        println!("{}", line(row));
    }
}

// ---------------------------------------------------------------- the baseline

/// Compare the result against the recorded baseline.
fn print_verdicts(ctx: &Context, results: &[CaseResult]) -> Result<(), String> {
    let path = ctx.suite.join("baseline.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let baseline = json::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("verdict against the recorded baseline ({})", path.display());
    let recorded_profile = baseline
        .get("machine")
        .and_then(|m| m.get("profile"))
        .and_then(|p| p.as_str())
        .unwrap_or("");
    if recorded_profile != ctx.machine.profile {
        println!(
            "  no verdict: the baseline was recorded on machine profile \
             '{recorded_profile}' and this run is '{}'",
            ctx.machine.profile
        );
        println!("  a number from another machine is not a result.");
        return Ok(());
    }
    let cases = baseline.get("cases").cloned().unwrap_or(json::Json::Null);
    let headers = ["case", "baseline ns/op", "now ns/op", "change", "verdict"];
    let mut rows = Vec::new();
    for r in results {
        let Some(ns) = r.ns_per_op else { continue };
        let entry = cases.get(&r.case.rel);
        let Some(base_ns) = entry
            .and_then(|e| e.get("ns_per_op"))
            .and_then(|v| v.as_f64())
        else {
            rows.push(vec![
                r.case.rel.clone(),
                "-".to_string(),
                format!("{ns:.2}"),
                "-".to_string(),
                "no baseline".to_string(),
            ]);
            continue;
        };
        let noise = entry
            .and_then(|e| e.get("noise"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let threshold = 0.02_f64.max(3.0 * noise);
        let change = if base_ns > 0.0 {
            (ns - base_ns) / base_ns
        } else {
            0.0
        };
        let verdict = match change {
            c if c > threshold => Verdict::Regression,
            c if c < -threshold => Verdict::Improvement,
            _ => Verdict::Unchanged,
        };
        rows.push(vec![
            r.case.rel.clone(),
            format!("{base_ns:.2}"),
            format!("{ns:.2}"),
            format!("{:+.1}%", change * 100.0),
            match verdict {
                Verdict::Improvement => "improvement".to_string(),
                Verdict::Regression => "REGRESSION".to_string(),
                Verdict::Unchanged => "no change".to_string(),
            },
        ]);
    }
    print_table(&headers, &rows, &[1, 2, 3]);
    Ok(())
}

/// The baseline file: the machine, the flags, and the numbers.
fn baseline_json(ctx: &Context, results: &[CaseResult]) -> String {
    let m = &ctx.machine;
    let mut s = String::new();
    s.push_str("{\n");
    s.push_str("  \"schema\": 1,\n");
    s.push_str(&format!(
        "  \"recorded_at\": {},\n",
        json::quote(&timestamp())
    ));
    s.push_str("  \"machine\": {\n");
    s.push_str(&format!("    \"profile\": {},\n", json::quote(&m.profile)));
    s.push_str(&format!("    \"cpu\": {},\n", json::quote(&m.cpu)));
    s.push_str(&format!("    \"cores\": {},\n", m.cores));
    s.push_str(&format!("    \"os\": {},\n", json::quote(&m.os)));
    s.push_str(&format!("    \"power\": {},\n", json::quote(&m.power)));
    s.push_str(&format!("    \"pinned\": {},\n", json::quote(&m.pinned)));
    s.push_str(&format!("    \"clang\": {},\n", json::quote(&m.clang)));
    s.push_str(&format!("    \"rustc\": {}\n", json::quote(&m.rustc)));
    s.push_str("  },\n");
    s.push_str("  \"flags\": {\n");
    s.push_str(&format!("    \"mir_opt\": {},\n", ctx.flags.mir_opt));
    s.push_str(&format!("    \"clang_opt\": {},\n", ctx.flags.clang_opt));
    s.push_str(&format!("    \"lto\": {},\n", ctx.flags.lto));
    s.push_str(&format!(
        "    \"clang_line\": {}\n",
        json::quote(&ctx.flags.clang_line())
    ));
    s.push_str("  },\n");
    s.push_str("  \"cases\": {\n");
    let mut first = true;
    for r in results {
        let Some(ns) = r.ns_per_op else { continue };
        if !first {
            s.push_str(",\n");
        }
        first = false;
        s.push_str(&format!("    {}: {{\n", json::quote(&r.case.rel)));
        s.push_str(&format!("      \"ns_per_op\": {ns:.4},\n"));
        s.push_str(&format!("      \"unit\": {},\n", json::quote(&r.case.unit)));
        s.push_str(&format!(
            "      \"ops\": {},\n",
            r.case.ops.unwrap_or_default()
        ));
        s.push_str(&format!("      \"runs\": {},\n", r.samples_ms.len()));
        s.push_str(&format!("      \"noise\": {:.5},\n", r.stats.noise));
        s.push_str(&format!(
            "      \"c_ns_per_op\": {},\n",
            match r.c_ns_per_op {
                Some(v) => format!("{v:.4}"),
                None => "null".to_string(),
            }
        ));
        s.push_str(&format!(
            "      \"comparison\": {},\n",
            match &r.comparison {
                Some(c) => json::quote(c),
                None => "null".to_string(),
            }
        ));
        s.push_str("      \"samples_ms\": [");
        for (i, sample) in r.samples_ms.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&format!("{sample:.3}"));
        }
        s.push_str("]\n");
        s.push_str("    }");
    }
    s.push('\n');
    s.push_str("  }\n");
    s.push_str("}\n");
    s
}

/// Write the raw run to `results/<timestamp>.json` and the human report to
/// `RESULTS.md`.
fn write_results(
    ctx: &Context,
    results: &[CaseResult],
    blocked: &[CaseResult],
) -> Result<(), String> {
    let dir = ctx.suite.join("results");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = timestamp();
    let path = dir.join(format!("{}.json", stamp.replace(':', "-")));
    fs::write(&path, baseline_json(ctx, results))
        .map_err(|e| format!("{}: {e}", path.display()))?;

    let mut md = String::new();
    md.push_str("# Benchmark results\n\n");
    md.push_str(&format!("Generated by `nx-bench` at {stamp}.\n\n"));
    md.push_str(&format!("* machine: {}\n", ctx.machine.line()));
    md.push_str(&format!("* profile: `{}`\n", ctx.machine.profile));
    md.push_str(&format!(
        "* flags: mir_opt={}, {}\n",
        ctx.flags.mir_opt,
        ctx.flags.clang_line()
    ));
    md.push_str("\nA number without the machine and the flags is not a result.\n\n");
    md.push_str("| case | unit | ops | ns/op | median (ms) | noise | C ns/op | ratio |\n");
    md.push_str("| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |\n");
    for r in results {
        let Some(ns) = r.ns_per_op else { continue };
        let c = r.c_ns_per_op;
        let ratio = match (c, ns) {
            (Some(c), _) if c > 0.0 => format!("{:.2}x", ns / c),
            _ => "-".to_string(),
        };
        md.push_str(&format!(
            "| `{}` | {} | {} | {:.2} | {:.1} | {:.3} | {} | {} |\n",
            r.case.rel,
            r.case.unit,
            r.case.ops.unwrap_or_default(),
            ns,
            r.stats.median,
            r.stats.noise,
            match c {
                Some(v) => format!("{v:.2}"),
                None => "-".to_string(),
            },
            ratio
        ));
    }
    if !blocked.is_empty() {
        md.push_str("\n## Not yet measurable\n\n");
        for r in blocked {
            md.push_str(&format!(
                "* `{}`: {}\n",
                r.case.rel,
                r.blocked.clone().unwrap_or_default()
            ));
        }
    }
    let md_path = ctx.suite.join("RESULTS.md");
    fs::write(&md_path, md).map_err(|e| format!("{}: {e}", md_path.display()))?;
    Ok(())
}

/// A UTC timestamp, without pulling in a date library.
fn timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = now / 86_400;
    let secs = now % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Days since the epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn matches(case: &Case, pattern: Option<&str>) -> bool {
    match pattern {
        None => true,
        Some(p) => case.rel.contains(p),
    }
}

// ---------------------------------------------------------------- JSON

/// A minimal JSON reader and writer.
///
/// The baseline file is JSON because it is an artefact other tools have to
/// read, and the driver has no JSON dependency, so the few hundred lines
/// that a reader costs live here.
mod json {
    #[derive(Debug, Clone, PartialEq)]
    pub enum Json {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<Json>),
        Obj(Vec<(String, Json)>),
    }

    impl Json {
        pub fn get(&self, key: &str) -> Option<&Json> {
            match self {
                Json::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Json::Str(s) => Some(s),
                _ => None,
            }
        }

        pub fn as_f64(&self) -> Option<f64> {
            match self {
                Json::Num(n) => Some(*n),
                _ => None,
            }
        }
    }

    pub fn parse(text: &str) -> Result<Json, String> {
        let chars: Vec<char> = text.chars().collect();
        let mut p = Parser { chars, i: 0 };
        p.skip_ws();
        let value = p.value()?;
        p.skip_ws();
        if p.i != p.chars.len() {
            return Err(format!("trailing input at character {}", p.i));
        }
        Ok(value)
    }

    struct Parser {
        chars: Vec<char>,
        i: usize,
    }

    impl Parser {
        fn skip_ws(&mut self) {
            while self.i < self.chars.len() && self.chars[self.i].is_whitespace() {
                self.i += 1;
            }
        }

        fn peek(&self) -> Option<char> {
            self.chars.get(self.i).copied()
        }

        fn expect(&mut self, c: char) -> Result<(), String> {
            if self.peek() == Some(c) {
                self.i += 1;
                Ok(())
            } else {
                Err(format!("expected '{c}' at character {}", self.i))
            }
        }

        fn value(&mut self) -> Result<Json, String> {
            match self.peek() {
                Some('{') => self.object(),
                Some('[') => self.array(),
                Some('"') => Ok(Json::Str(self.string()?)),
                Some('t') => self.literal("true", Json::Bool(true)),
                Some('f') => self.literal("false", Json::Bool(false)),
                Some('n') => self.literal("null", Json::Null),
                Some(_) => self.number(),
                None => Err("unexpected end of input".to_string()),
            }
        }

        fn literal(&mut self, word: &str, value: Json) -> Result<Json, String> {
            for c in word.chars() {
                self.expect(c)?;
            }
            Ok(value)
        }

        fn object(&mut self) -> Result<Json, String> {
            self.expect('{')?;
            let mut fields = Vec::new();
            self.skip_ws();
            if self.peek() == Some('}') {
                self.i += 1;
                return Ok(Json::Obj(fields));
            }
            loop {
                self.skip_ws();
                let key = self.string()?;
                self.skip_ws();
                self.expect(':')?;
                self.skip_ws();
                let value = self.value()?;
                fields.push((key, value));
                self.skip_ws();
                match self.peek() {
                    Some(',') => self.i += 1,
                    Some('}') => {
                        self.i += 1;
                        return Ok(Json::Obj(fields));
                    }
                    _ => return Err(format!("expected ',' or '}}' at character {}", self.i)),
                }
            }
        }

        fn array(&mut self) -> Result<Json, String> {
            self.expect('[')?;
            let mut items = Vec::new();
            self.skip_ws();
            if self.peek() == Some(']') {
                self.i += 1;
                return Ok(Json::Arr(items));
            }
            loop {
                self.skip_ws();
                items.push(self.value()?);
                self.skip_ws();
                match self.peek() {
                    Some(',') => self.i += 1,
                    Some(']') => {
                        self.i += 1;
                        return Ok(Json::Arr(items));
                    }
                    _ => return Err(format!("expected ',' or ']' at character {}", self.i)),
                }
            }
        }

        fn string(&mut self) -> Result<String, String> {
            self.expect('"')?;
            let mut out = String::new();
            while let Some(c) = self.peek() {
                self.i += 1;
                match c {
                    '"' => return Ok(out),
                    '\\' => {
                        let esc = self
                            .peek()
                            .ok_or_else(|| "unterminated escape".to_string())?;
                        self.i += 1;
                        match esc {
                            'n' => out.push('\n'),
                            't' => out.push('\t'),
                            'r' => out.push('\r'),
                            'b' => out.push('\u{8}'),
                            'f' => out.push('\u{c}'),
                            'u' => {
                                let hex: String = self.chars
                                    [self.i..(self.i + 4).min(self.chars.len())]
                                    .iter()
                                    .collect();
                                self.i += 4;
                                let code = u32::from_str_radix(&hex, 16)
                                    .map_err(|_| format!("bad escape '\\u{hex}'"))?;
                                out.push(char::from_u32(code).unwrap_or('?'));
                            }
                            other => out.push(other),
                        }
                    }
                    other => out.push(other),
                }
            }
            Err("unterminated string".to_string())
        }

        fn number(&mut self) -> Result<Json, String> {
            let start = self.i;
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() || "+-.eE".contains(c) {
                    self.i += 1;
                } else {
                    break;
                }
            }
            let text: String = self.chars[start..self.i].iter().collect();
            text.parse::<f64>()
                .map(Json::Num)
                .map_err(|_| format!("bad number '{text}'"))
        }
    }

    /// Quote a string as a JSON string.
    pub fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

// ---------------------------------------------------------------- the tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_is_parsed() {
        let header = parse_header(
            "# BENCH unit=ns-per-call ops=1000000 expect=3628800 ir_check=call i64 @noinline\n\
             fn main():\n    print(1)\n",
        )
        .expect("the header parses");
        assert_eq!(header.unit.as_deref(), Some("ns-per-call"));
        assert_eq!(header.ops, Some(1_000_000));
        assert_eq!(header.expect.as_deref(), Some("3628800"));
        assert_eq!(header.ir_check.as_deref(), Some("call i64 @noinline"));
        assert!(header.base.is_none());
    }

    #[test]
    fn a_quoted_note_keeps_its_spaces() {
        let header = parse_header("# BENCH unit=call note=\"the call, by value\" ops=2\n")
            .expect("the header parses");
        assert_eq!(header.note.as_deref(), Some("the call, by value"));
        assert_eq!(header.ops, Some(2));
    }

    #[test]
    fn a_header_after_a_comment_block_is_found() {
        let header = parse_header("# a comment\n# BENCH unit=call ops=7 expect=7\nfn main():\n")
            .expect("the header parses");
        assert_eq!(header.ops, Some(7));
    }

    #[test]
    fn a_file_without_a_header_is_rejected() {
        assert!(parse_header("fn main():\n    print(1)\n").is_err());
    }

    #[test]
    fn an_unknown_field_is_rejected() {
        assert!(parse_header("# BENCH unit=call ops=1 wat=2\n").is_err());
    }

    #[test]
    fn the_statistics_use_the_median_and_the_interquartile_range() {
        let samples: Vec<f64> = (1..=15).map(|n| n as f64).collect();
        let stats = Stats::of(&samples);
        assert_eq!(stats.median, 8.0);
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 15.0);
        assert_eq!(stats.p25, 4.0);
        assert_eq!(stats.p75, 12.0);
        assert!((stats.noise - 8.0 / 8.0).abs() < 1e-12);
    }

    #[test]
    fn the_statistics_do_not_care_about_the_order() {
        let mut samples: Vec<f64> = (1..=15).map(|n| n as f64).collect();
        samples.reverse();
        let stats = Stats::of(&samples);
        assert_eq!(stats.median, 8.0);
    }

    #[test]
    fn the_checksum_comparison_is_exact_for_integers() {
        assert!(expect_matches("3628800", "3628800"));
        assert!(!expect_matches("3628801", "3628800"));
    }

    #[test]
    fn the_checksum_comparison_has_a_tolerance_for_floats() {
        assert!(expect_matches("1.5", "1.5"));
        assert!(expect_matches("1.5000000000001", "1.5"));
        assert!(!expect_matches("1.6", "1.5"));
    }

    #[test]
    fn the_last_line_is_the_checksum() {
        assert_eq!(checksum("noise\n42\n"), Some("42".to_string()));
        assert_eq!(checksum(""), None);
    }

    #[test]
    fn json_round_trips() {
        let text = "{\"a\": 1, \"b\": [1, 2, 3], \"c\": \"x\\ny\", \"d\": null, \"e\": true}";
        let value = json::parse(text).expect("the json parses");
        assert_eq!(value.get("a").and_then(|v| v.as_f64()), Some(1.0));
        assert_eq!(
            value.get("b").and_then(|v| match v {
                json::Json::Arr(items) => Some(items.len()),
                _ => None,
            }),
            Some(3)
        );
        assert_eq!(value.get("c").and_then(|v| v.as_str()), Some("x\ny"));
        assert_eq!(value.get("d"), Some(&json::Json::Null));
    }

    #[test]
    fn json_strings_are_quoted() {
        assert_eq!(json::quote("a\"b\\c\nd"), "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn a_call_in_the_assembly_is_found() {
        assert!(assembly_calls("\tcallq\t_noinline\n\tretq\n", "noinline"));
        assert!(assembly_calls("\tjmp\tnoinline\n", "noinline"));
        assert!(!assembly_calls("\tcallq\tprintf\n", "noinline"));
    }

    #[test]
    fn the_timestamp_is_a_utc_date() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 20);
        assert!(stamp.ends_with('Z'));
        assert_eq!(&stamp[4..5], "-");
    }
}
