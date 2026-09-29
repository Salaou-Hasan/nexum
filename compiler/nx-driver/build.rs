fn main() {
    // Expose the cargo target triple to the binary for self-update URLs.
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=NX_TARGET={target}");
    println!("cargo:rerun-if-env-changed=TARGET");
}
