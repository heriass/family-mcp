//! The TypeScript package's own cases against this binary: tests/ts holds copies of
//! packages/inna-mcp/test suites, changed only to start this binary (the tests/ts/rust-inna.ts
//! drop-ins) and marked `Rust:` where they differ. `FAMILY_MCP_BUN` must name a Bun 1.4.2
//! executable: these tests fail without it and are never skipped. They need the `test-origin`
//! feature, without which the binary would talk to Inna itself.
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
        // Fixtures are found relative to the TypeScript package.
        .current_dir(manifest.join("../../packages/inna-mcp"))
        .env("INNA_RUST_BINARY", env!("CARGO_BIN_EXE_inna-mcp"))
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
fn the_integration_cases_pass_against_this_binary() {
    bun_test("integration.test.ts", 54);
}

#[test]
fn the_electronic_id_login_cases_pass_against_this_binary() {
    bun_test("login.test.ts", 3);
}

#[test]
fn the_fake_browser_cases_pass_against_this_binary() {
    bun_test("browser-login.test.ts", 16);
}

#[test]
fn the_startup_cases_pass_against_this_binary() {
    bun_test(
        "startup.test.ts",
        match cfg!(target_os = "macos") {
            true => 2,
            false => 1,
        },
    );
}

#[test]
fn renewal_and_bounded_recovery_pass_against_this_binary() {
    bun_test("renewal.test.ts", 11);
}
