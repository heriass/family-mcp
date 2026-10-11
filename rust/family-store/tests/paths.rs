//! packages/session-store/test/paths.test.ts: where the store lives, from a named environment.

mod common;

use std::path::{Path, PathBuf};

use common::code;
use family_store::{
    Code, StoreEnvironment, default_secret_record_path_in, key_provider_in, retired_store_paths_in,
};

const MAC_ROOT: &str = "/Users/scratch/Library/Application Support/family-mcp";

/// Production on `os`: the test seam off, a scratch HOME, and no XDG or key backend.
fn production(os: &str) -> StoreEnvironment {
    StoreEnvironment {
        os: os.to_owned(),
        home: Some(PathBuf::from("/Users/scratch")),
        ..StoreEnvironment::default()
    }
}

fn with_xdg(mut environment: StoreEnvironment, config: &str, data: &str) -> StoreEnvironment {
    environment.xdg_config_home = Some(config.into());
    environment.xdg_data_home = Some(data.into());
    environment
}

fn key_path(environment: &StoreEnvironment) -> PathBuf {
    let keys = key_provider_in(environment, "test-mcp", "work").unwrap();
    assert_eq!(keys.key_source(), "local-file");
    assert_eq!(keys.key_id(), "local");
    keys.key_file().unwrap().to_owned()
}

fn record_path(environment: &StoreEnvironment) -> PathBuf {
    default_secret_record_path_in(environment, "test-mcp").unwrap()
}

fn at(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[test]
fn macos_keeps_keys_and_records_under_application_support_and_ignores_xdg() {
    for environment in [
        production("macos"),
        with_xdg(production("macos"), "/config", "/data"),
        with_xdg(production("macos"), "relative/config", "relative/data"),
    ] {
        assert_eq!(
            key_path(&environment),
            Path::new(MAC_ROOT).join("keys/test-mcp.work.key")
        );
        assert_eq!(
            record_path(&environment),
            Path::new(MAC_ROOT).join("test-mcp/session.enc")
        );
    }
}

#[test]
fn linux_keeps_its_xdg_paths() {
    let linux = production("linux");
    assert_eq!(
        key_path(&linux),
        at("/Users/scratch/.local/share/family-mcp/keys/test-mcp.work.key")
    );
    assert_eq!(
        record_path(&linux),
        at("/Users/scratch/.config/test-mcp/session.enc")
    );

    let configured = with_xdg(production("linux"), "/config", "/data");
    assert_eq!(
        key_path(&configured),
        at("/data/family-mcp/keys/test-mcp.work.key")
    );
    assert_eq!(record_path(&configured), at("/config/test-mcp/session.enc"));

    // Both XDG roots are absolute, so an unusable HOME does not matter.
    let mut homeless = with_xdg(production("linux"), "/config", "/data");
    homeless.home = Some(at("relative"));
    assert_eq!(
        key_path(&homeless),
        at("/data/family-mcp/keys/test-mcp.work.key")
    );

    let mut relative = production("linux");
    relative.xdg_data_home = Some("relative/data".into());
    assert_eq!(
        key_path(&relative),
        at("/Users/scratch/.local/share/family-mcp/keys/test-mcp.work.key")
    );
}

#[test]
fn the_test_seam_makes_macos_follow_xdg_like_linux() {
    let mut seam = with_xdg(production("macos"), "/config", "/data");
    seam.test_seam = true;
    assert_eq!(
        key_path(&seam),
        at("/data/family-mcp/keys/test-mcp.work.key")
    );
    assert_eq!(record_path(&seam), at("/config/test-mcp/session.enc"));
    assert_eq!(
        retired_store_paths_in(&seam, "test-mcp", "default").unwrap(),
        Vec::<PathBuf>::new()
    );
}

/// Only the exact value turns the seam on; a child process reads it from its environment.
#[test]
fn only_one_value_turns_the_seam_on() {
    for (value, on) in [("1", true), ("true", false), ("", false), ("0", false)] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "print_the_seam", "--ignored", "--nocapture"])
            .env(family_store::TEST_SEAM, value)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("seam:{on}\n")),
            "{value}: {stdout}"
        );
    }
}

#[test]
#[ignore = "run by only_one_value_turns_the_seam_on"]
fn print_the_seam() {
    println!("seam:{}", family_store::test_seam());
}

#[test]
fn a_relative_or_unknown_home_is_refused() {
    for home in [Some("relative"), Some("relative/home"), Some("."), None] {
        for os in ["macos", "linux"] {
            let mut environment = production(os);
            environment.home = home.map(PathBuf::from);
            let refused = |error: family_store::Error| {
                assert_eq!(error.code, Code::StoreUnavailable);
                assert_eq!(
                    error.message,
                    "The home directory is not known; set HOME to an absolute path."
                );
            };
            refused(
                key_provider_in(&environment, "test-mcp", "default")
                    .err()
                    .unwrap(),
            );
            refused(default_secret_record_path_in(&environment, "test-mcp").unwrap_err());
        }
    }
}

#[test]
fn names_are_checked_and_keys_is_never_a_store_name() {
    for os in ["macos", "linux"] {
        let environment = production(os);
        let invalid = Some(Code::InvalidArgument);
        assert_eq!(
            code(default_secret_record_path_in(&environment, "../escape")),
            invalid
        );
        assert_eq!(
            code(default_secret_record_path_in(&environment, "keys")),
            invalid
        );
        assert_eq!(
            code(key_provider_in(&environment, "../x", "default")),
            invalid
        );
    }
}

#[test]
fn the_retired_backend_variable_accepts_only_file_or_nothing() {
    for value in ["", "file"] {
        let mut environment = production("macos");
        environment.key_backend = Some(value.into());
        assert_eq!(
            key_path(&environment),
            Path::new(MAC_ROOT).join("keys/test-mcp.work.key")
        );
    }

    for value in ["keychain", "keychain-accessor", "FILE", " file", "0"] {
        for os in ["macos", "linux"] {
            let mut environment = production(os);
            environment.key_backend = Some(value.into());
            let error = key_provider_in(&environment, "test-mcp", "default")
                .err()
                .unwrap();
            assert_eq!(error.code, Code::StoreUnavailable);
            assert_eq!(
                error.message,
                "FAMILY_MCP_KEY_BACKEND is not supported. Set it to file or unset it."
            );
        }
    }
    assert_eq!(
        code(key_provider_in(
            &production("windows"),
            "test-mcp",
            "default"
        )),
        Some(Code::StoreUnavailable)
    );
}

#[test]
fn retired_macos_layouts_are_named_by_path_only() {
    assert_eq!(
        retired_store_paths_in(&production("macos"), "test-mcp", "default").unwrap(),
        [
            at("/Users/scratch/.config/test-mcp/session.enc"),
            at("/Users/scratch/.config/test-mcp/session.enc.marker"),
            at("/Users/scratch/.config/test-mcp/session.enc.lock"),
            at("/Users/scratch/.local/share/family-mcp/keys/test-mcp.default.key"),
        ]
    );
    assert_eq!(
        retired_store_paths_in(&production("linux"), "test-mcp", "default").unwrap(),
        Vec::<PathBuf>::new()
    );

    // An earlier build honoured absolute XDG directories on macOS too.
    let paths = retired_store_paths_in(
        &with_xdg(production("macos"), "/config", "/data"),
        "test-mcp",
        "default",
    )
    .unwrap();
    assert!(paths.contains(&at("/config/test-mcp/session.enc.marker")));
    assert!(paths.contains(&at("/data/family-mcp/keys/test-mcp.default.key")));
    assert!(paths.contains(&at("/Users/scratch/.config/test-mcp/session.enc")));
}

#[test]
fn xdg_variables_that_point_into_application_support_never_name_the_current_store() {
    let old = [
        at("/Users/scratch/.config/test-mcp/session.enc"),
        at("/Users/scratch/.config/test-mcp/session.enc.marker"),
        at("/Users/scratch/.config/test-mcp/session.enc.lock"),
        at("/Users/scratch/.local/share/family-mcp/keys/test-mcp.default.key"),
    ];

    for (config, data) in [
        (
            MAC_ROOT.to_owned(),
            "/Users/scratch/Library/Application Support".to_owned(),
        ),
        (
            format!("{MAC_ROOT}/"),
            "/Users/scratch/Library/./Application Support/".to_owned(),
        ),
        (
            format!("{MAC_ROOT}/../family-mcp"),
            "/Users/scratch/Library/Application Support//".to_owned(),
        ),
    ] {
        let environment = with_xdg(production("macos"), &config, &data);
        assert_eq!(
            retired_store_paths_in(&environment, "test-mcp", "default").unwrap(),
            old,
            "{config} {data}"
        );
    }

    // Only one of them overlapping: the other one's old paths stay.
    let environment = with_xdg(production("macos"), MAC_ROOT, "/data");
    let mut expected = old.to_vec();
    expected.push(at("/data/family-mcp/keys/test-mcp.default.key"));
    assert_eq!(
        retired_store_paths_in(&environment, "test-mcp", "default").unwrap(),
        expected
    );
}

#[test]
fn no_source_file_reaches_the_keychain_or_security() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

    for entry in std::fs::read_dir(source).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains("/usr/bin/security"));
        assert!(!text.contains("find-generic-password"));
    }
}

/// The cases above name their environment; this one reads it as a server does,
/// `StoreEnvironment::current()` in a child given HOME, XDG and FAMILY_MCP_KEY_BACKEND, as the
/// TypeScript cases set them.
#[test]
fn a_server_reads_home_xdg_and_the_key_backend_from_its_environment() {
    let mac = cfg!(target_os = "macos");
    for (seam, backend, expected) in [
        (
            "",
            "",
            if mac {
                "/Users/scratch/Library/Application Support/family-mcp/keys/test-mcp.work.key \
                 /Users/scratch/Library/Application Support/family-mcp/test-mcp/session.enc"
            } else {
                "/data/family-mcp/keys/test-mcp.work.key /config/test-mcp/session.enc"
            },
        ),
        (
            "1",
            "file",
            "/data/family-mcp/keys/test-mcp.work.key /config/test-mcp/session.enc",
        ),
        (
            "",
            "keychain",
            "error:FAMILY_MCP_KEY_BACKEND is not supported. Set it to file or unset it.",
        ),
    ] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "print_the_store", "--ignored", "--nocapture"])
            .env("HOME", "/Users/scratch")
            .env("XDG_CONFIG_HOME", "/config")
            .env("XDG_DATA_HOME", "/data")
            .env("FAMILY_MCP_KEY_BACKEND", backend)
            .env(family_store::TEST_SEAM, seam)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("store:{expected}\n")),
            "{seam} {backend}: {stdout}"
        );
    }
}

#[test]
#[ignore = "run by a_server_reads_home_xdg_and_the_key_backend_from_its_environment"]
fn print_the_store() {
    let environment = StoreEnvironment::current();
    match key_provider_in(&environment, "test-mcp", "work") {
        Ok(keys) => println!(
            "store:{} {}",
            keys.key_file().unwrap().display(),
            record_path(&environment).display()
        ),
        Err(error) => println!("store:error:{}", error.message),
    }
}
