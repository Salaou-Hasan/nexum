//! Target descriptors and capability model (bootstrap).

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub triple: String,
    pub pointer_width: u32,
    pub os: String,
    pub arch: String,
}

impl TargetSpec {
    /// The target the compiler runs on. It decides which runtime the
    /// generated program gets, so it has to follow the host.
    pub fn host() -> Self {
        let arch = std::env::consts::ARCH;
        match std::env::consts::OS {
            "windows" => Self {
                triple: format!("{arch}-pc-windows-msvc"),
                pointer_width: 64,
                os: "windows".to_string(),
                arch: arch.to_string(),
            },
            "macos" => Self {
                triple: format!("{arch}-apple-darwin"),
                pointer_width: 64,
                os: "macos".to_string(),
                arch: arch.to_string(),
            },
            _ => Self {
                triple: format!("{arch}-unknown-linux-gnu"),
                pointer_width: 64,
                os: "linux".to_string(),
                arch: arch.to_string(),
            },
        }
    }

    /// Resolve a named target profile.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "windows-x86_64" | "x86_64-pc-windows-msvc" => Some(Self {
                triple: "x86_64-pc-windows-msvc".to_string(),
                pointer_width: 64,
                os: "windows".to_string(),
                arch: "x86_64".to_string(),
            }),
            "linux-x86_64" | "x86_64-unknown-linux-gnu" => Some(Self {
                triple: "x86_64-unknown-linux-gnu".to_string(),
                pointer_width: 64,
                os: "linux".to_string(),
                arch: "x86_64".to_string(),
            }),
            "linux-aarch64" | "aarch64-unknown-linux-gnu" => Some(Self {
                triple: "aarch64-unknown-linux-gnu".to_string(),
                pointer_width: 64,
                os: "linux".to_string(),
                arch: "aarch64".to_string(),
            }),
            "macos-x86_64" | "x86_64-apple-darwin" => Some(Self {
                triple: "x86_64-apple-darwin".to_string(),
                pointer_width: 64,
                os: "macos".to_string(),
                arch: "x86_64".to_string(),
            }),
            "macos-aarch64" | "aarch64-apple-darwin" => Some(Self {
                triple: "aarch64-apple-darwin".to_string(),
                pointer_width: 64,
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            }),
            _ => None,
        }
    }

    pub fn list() -> &'static [&'static str] {
        &[
            "windows-x86_64",
            "linux-x86_64",
            "linux-aarch64",
            "macos-x86_64",
            "macos-aarch64",
        ]
    }

    pub fn data_layout_str(&self) -> &'static str {
        match (self.arch.as_str(), self.os.as_str()) {
            ("x86_64", "windows") => {
                "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
            }
            ("x86_64", "linux") => {
                "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
            }
            ("x86_64", "macos") => {
                "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
            }
            ("aarch64", "linux") => "e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128",
            ("aarch64", "macos") => "e-m:o-i64:64-i128:128-n32:64-S128",
            _ => "e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128",
        }
    }

    pub fn is_host(&self) -> bool {
        self.triple == Self::host().triple
    }
}
