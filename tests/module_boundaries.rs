//! Source-level guards for boundaries the type system can't express.
//!
//! Both rules below are stated in `src/CLAUDE.md` and were being
//! violated in dozens of places before they were checked mechanically.

use std::fs;
use std::path::{Path, PathBuf};

/// Every `.rs` file under `src/`.
fn source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("readable source dir") {
            let path = entry.expect("readable dir entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new("src"), &mut out);
    out
}

#[test]
fn kurbo_and_peniko_are_reached_through_our_own_module_paths() {
    // The wrapper modules are the single place a backend-type swap
    // has to touch, which only holds if nothing else names the
    // upstream crates. Backends are exempt: mapping our restricted
    // enums onto the native ones is their job.
    const EXEMPT: &[&str] = &[
        "src/geometry.rs",
        "src/path.rs",
        "src/brush.rs",
        "src/color.rs",
        "src/stroke.rs",
        "src/mesh.rs",
        "src/scene/mod.rs",
        "src/backend/",
    ];

    let mut offenders = Vec::new();
    for file in source_files() {
        let display = file.to_string_lossy().replace('\\', "/");
        if EXEMPT.iter().any(|e| display.starts_with(e)) {
            continue;
        }
        for (i, line) in fs::read_to_string(&file)
            .expect("readable source file")
            .lines()
            .enumerate()
        {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("kurbo::") || code.contains("peniko::") {
                offenders.push(format!("{display}:{}: {}", i + 1, line.trim()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "reach these through `crate::geometry` / `crate::path` / `crate::brush` \
         instead:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scales_layer_stays_free_of_the_plot_layer() {
    // `src/scales/` is meant to lift out into its own crate as-is.
    let mut offenders = Vec::new();
    for file in source_files() {
        let display = file.to_string_lossy().replace('\\', "/");
        if !display.starts_with("src/scales/") {
            continue;
        }
        for (i, line) in fs::read_to_string(&file)
            .expect("readable source file")
            .lines()
            .enumerate()
        {
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for forbidden in [
                "crate::plot",
                "crate::scene",
                "crate::backend",
                "crate::primitives",
                "crate::text",
            ] {
                if code.contains(forbidden) {
                    offenders.push(format!("{display}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "src/scales/ must not depend on the layers above it:\n{}",
        offenders.join("\n")
    );
}
