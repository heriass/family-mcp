//! The cases of tests/ts/startup.test.ts, executed against this binary without Bun: the startup
//! store check refuses an unsafe store before anything is touched, and an earlier build's macOS
//! store keeps deciding. Needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use std::fs::{self, DirBuilder};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

use support::{Home, Run, cli};

/// A closed port: nothing here may reach Krónan.
const NOWHERE: &str = "http://127.0.0.1:9";

const E3: &str = "Other users can open this store folder.";

#[test]
fn the_cli_refuses_to_start_on_an_unsafe_store_and_touches_nothing() {
    let home = Home::new("startup");

    for relative in [".config/kronan-mcp", ".local/share/family-mcp/keys"] {
        let directory = home.path(relative);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&directory)
            .unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

        for args in [&["serve"][..], &["auth", "logout"]] {
            let shown = format!("<home>/{relative}");
            assert_eq!(
                cli(&home, NOWHERE, &[], args, None),
                Run {
                    code: Some(1),
                    stdout: String::new(),
                    stderr: format!(
                        "kronan-mcp: cannot start. {E3}\n  Path: '{shown}'\n  Fix:  chmod 700 '{shown}'\n"
                    ),
                },
                "{args:?}"
            );
            assert_eq!(fs::read_dir(&directory).unwrap().count(), 0, "{args:?}");
        }

        // Help and version never touch the store.
        for flag in ["--help", "--version"] {
            assert_eq!(cli(&home, NOWHERE, &[], &[flag], None).code, Some(0));
        }
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

// The production macOS layout under a scratch HOME in the temporary directory: the store test seam
// is off, as the drop-in case ran it. The key is a file there too; nothing reaches the real store.
#[cfg(target_os = "macos")]
#[test]
fn an_earlier_builds_macos_store_keeps_deciding() {
    let home = Home::new("transition");
    let legacy = home.path("legacy-session.json");
    let marker = ".config/kronan-mcp/session.enc.marker";
    home.write(marker, "old", None);
    home.write("legacy-session.json", "{}", None);

    let output = std::process::Command::new(support::BINARY)
        .args(["auth", "migrate"])
        .current_dir(&home.0)
        .env_clear()
        .envs([
            ("PATH", "/usr/bin:/bin"),
            ("HOME", home.0.to_str().unwrap()),
            ("KRONAN_TOKEN_FILE", legacy.to_str().unwrap()),
            ("KRONAN_TEST_ORIGIN", NOWHERE),
            // The binary accepts the seam-off layout only under its temporary directory.
            ("TMPDIR", std::env::temp_dir().to_str().unwrap()),
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("store key is missing"), "{stderr}");
    assert_eq!(fs::read_to_string(&legacy).unwrap(), "{}");
    assert_eq!(fs::read_to_string(home.path(marker)).unwrap(), "old");
}
