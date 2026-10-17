//! Target descriptors and capability model (bootstrap).

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub triple: String,
    pub pointer_width: u32,
    pub os: String,
    pub arch: String,
}

impl TargetSpec {
    pub fn host() -> Self {
        // Windows x86_64
        Self {
            triple: "x86_64-pc-windows-msvc".to_string(),
            pointer_width: 64,
            os: "windows".to_string(),
            arch: "x86_64".to_string(),
        }
    }
    pub fn data_layout_str(&self) -> &'static str {
        "e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128"
    }
}
