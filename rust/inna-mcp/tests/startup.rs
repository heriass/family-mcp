//! The store check before the CLI starts, executed: tests/ts/startup.test.ts against the binary.
//! Each run has a scratch HOME and nothing else of the test's environment. The macOS case turns
//! the store test seam off, as the TypeScript case did: the production macOS layout, under the
//! scratch HOME, whose key is a file there (family-store never reaches the Keychain). Needs no
//! Bun; needs the `test-origin` feature, whose origin here is a closed loopback port.
#![cfg(feature = "test-origin")]

mod support;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};

use support::{BINARY, Home};

const E3: &str = "Other users can open this store folder.";

/// startup.test.ts's `run`: the CLI against a scratch store, with stdin closed.
fn run(home: &Path, vars: &[(&str, String)], args: &[&str]) -> (String, String, Option<i32>) {
    let output = Command::new(BINARY)
        .args(args)
        .current_dir(home)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("TMPDIR", home)
        .env("INNA_TEST_ORIGIN", "http://127.0.0.1:9")
        .envs(vars.iter().map(|(name, value)| (*name, value)))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
        output.status.code(),
    )
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn the_cli_refuses_to_start_on_an_unsafe_store_and_touches_nothing() {
    let home = Home::new("startup");
    let vars = [
        ("FAMILY_MCP_STORE_TEST_SEAM", "1".to_owned()),
        ("XDG_CONFIG_HOME", home.path("config").display().to_string()),
        ("XDG_DATA_HOME", home.path("data").display().to_string()),
    ];

    for directory in [
        home.path("config/inna-mcp"),
        home.path("data/family-mcp/keys"),
    ] {
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)
            .unwrap();
        chmod(&directory, 0o755);
        let shown = directory.display();

        for args in [&["serve"][..], &["auth", "logout"][..]] {
            assert_eq!(
                run(&home.0, &vars, args),
                (
                    String::new(),
                    format!(
                        "inna-mcp: cannot start. {E3}\n  Path: '{shown}'\n  Fix:  chmod 700 '{shown}'\n"
                    ),
                    Some(1)
                ),
                "{args:?}"
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        }
        // Help and version never touch the store.
        for flag in ["--help", "--version"] {
            assert_eq!(run(&home.0, &vars, &[flag]).2, Some(0), "{flag}");
        }
        chmod(&directory, 0o700);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn an_earlier_builds_macos_store_keeps_deciding() {
    let home = Home::new("transition");
    let legacy = home.path("legacy-session.json");
    DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(home.path(".config/inna-mcp"))
        .unwrap();
    home.write(".config/inna-mcp/session.enc.marker", "old", 0o600);
    home.write("legacy-session.json", "{}", 0o600);

    let vars = [("INNA_SESSION_FILE", legacy.display().to_string())];
    let (_, stderr, code) = run(&home.0, &vars, &["auth", "migrate"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("store key is missing"), "{stderr}");
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "{}");
    assert_eq!(
        fs::read_to_string(home.path(".config/inna-mcp/session.enc.marker")).unwrap(),
        "old"
    );
}
