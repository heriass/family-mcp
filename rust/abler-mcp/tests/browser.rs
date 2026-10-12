//! `auth login` and `auth capture` against the TypeScript package's own fakes: tests/ts holds a
//! copy of packages/abler-mcp/test/browser-login.test.ts and the capture and loopback cases of its
//! other suites, changed only to start this binary, and a copy of integration.test.ts whose
//! in-process clients and CLI spawns are this binary (tests/ts/rust-abler.ts), and of
//! startup.test.ts. `FAMILY_MCP_BUN` must name a Bun 1.4.2
//! executable: these tests fail without it and are never skipped. They need the `test-origin`
//! feature, without which verification would talk to Abler itself.
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
        // The fakes and README are found relative to the TypeScript package.
        .current_dir(manifest.join("../../packages/abler-mcp"))
        .env("ABLER_RUST_BINARY", env!("CARGO_BIN_EXE_abler-mcp"))
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
fn the_fake_browser_cases_pass_against_this_binary() {
    bun_test("browser-login.test.ts", 14);
}

#[test]
fn the_capture_and_loopback_cases_pass_against_this_binary() {
    bun_test("capture.test.ts", 4);
}

#[test]
fn the_integration_cases_pass_against_this_binary() {
    bun_test("integration.test.ts", 24);
}

#[test]
fn the_startup_cases_pass_against_this_binary() {
    bun_test(
        "startup.test.ts",
        if cfg!(target_os = "macos") { 2 } else { 1 },
    );
}

#[test]
fn the_schema_discovery_cases_pass_against_this_binary() {
    bun_test("describe.test.ts", 2);
}

#[test]
fn the_guarded_field_discovery_cases_pass_against_this_binary() {
    bun_test("describe-fields.test.ts", 1);
}
