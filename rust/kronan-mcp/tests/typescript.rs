//! The TypeScript package's own suites against this binary: tests/ts holds copies of
//! packages/kronan-mcp/test files whose CLI spawns, in-process clients and auth functions are this
//! binary (tests/ts/rust-kronan.ts), changed only where marked `Rust:`. `FAMILY_MCP_BUN` must name
//! a Bun 1.4.2 executable: these tests fail without it and are never skipped. They need the
//! `test-origin` feature, without which the binary would talk to Krónan itself.
#![cfg(feature = "test-origin")]

mod scratch;

use std::path::Path;
use std::process::Command;

fn bun_test(file: &str, passed: usize) {
    let bun = std::env::var_os("FAMILY_MCP_BUN")
        .expect("FAMILY_MCP_BUN must name a Bun 1.4.2 executable; parity tests never skip");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = scratch::Scratch::new(file);
    let mut command = Command::new(bun);
    command
        .args(["test", "--timeout", "30000"])
        .arg(manifest.join("tests/ts").join(file))
        // Fixtures and the package's bunfig preload are found relative to the TypeScript package.
        .current_dir(manifest.join("../../packages/kronan-mcp"))
        .env("KRONAN_RUST_BINARY", env!("CARGO_BIN_EXE_kronan-mcp"))
        .env("BUN_RUNTIME_TRANSPILER_CACHE_PATH", "0");
    // The TypeScript package and every binary the cases start keep their store in scratch
    // directories (the package's bunfig preload sets the same).
    scratch.isolate(&mut command);
    let output = command.output().unwrap();
    let report = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "{}\n{report}",
        String::from_utf8_lossy(&output.stdout)
    );
    // Not vacuous: every case ran and none was skipped or filtered out.
    assert!(
        report.contains(&format!("\n {passed} pass\n")) && report.contains("\n 0 fail\n"),
        "{report}"
    );
    assert!(!report.contains(" skip\n"), "{report}");
}

#[test]
fn the_startup_cases_pass_against_this_binary() {
    bun_test(
        "startup.test.ts",
        if cfg!(target_os = "macos") { 2 } else { 1 },
    );
}

#[test]
fn the_integration_cases_pass_against_this_binary() {
    bun_test("integration.test.ts", 31);
}

#[test]
fn the_loopback_case_passes_against_this_binary() {
    bun_test("loopback.test.ts", 1);
}

#[test]
fn the_typescript_package_and_this_binary_share_the_token_store_and_attempts() {
    bun_test("interop.test.ts", 4);
}
