//! Names one cfg for "a target with the /proc- and job-control-shaped Unix
//! this crate's process observer needs" instead of repeating the same
//! eight-target exclusion list at every site that needs it.

fn main() {
    println!("cargo::rustc-check-cfg=cfg(unix_process_observer)");
    let excluded = [
        "cygwin",
        "emscripten",
        "fuchsia",
        "horizon",
        "netbsd",
        "openbsd",
        "redox",
        "wasi",
    ];
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if !excluded.contains(&target_os.as_str()) {
        println!("cargo::rustc-cfg=unix_process_observer");
    }
}
