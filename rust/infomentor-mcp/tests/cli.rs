//! Behavior parity.rs cannot compare with the TypeScript package: the version, the test build's
//! refusal of a non-local upstream, and the proxy variables Bun's fetch honours.
#![cfg(feature = "test-origin")]

use std::fs::{self, DirBuilder};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn debug_logs_each_redirect_without_secrets_and_is_off_unless_exactly_one() {
    for debug in [None, Some("0"), Some("true"), Some("1")] {
        let home = Home::new();
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", server.local_addr().unwrap());
        let serving = std::thread::spawn(move || {
            for step in 0..2 {
                let (mut stream, _) = server.accept().unwrap();
                let mut request = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    request.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() { break; }
                }
                if step == 0 {
                    stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: https://minn.infomentor.is/error?oauth_token=secret-token\r\nSet-Cookie: transaction=secret-cookie; Path=/; Secure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    let body = "<html><h1>Service Unavailable</h1><p>secret-user secret-password secret-cookie secret-token ?secret-query</p></html>";
                    write!(stream, "HTTP/1.1 503 Unavailable\r\nSet-Cookie: response=secret-response; Path=/; Secure\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let mut command = home.command(Some(&origin), &["login"]);
        command.env("INFOMENTOR_USERNAME", "secret-user").env("INFOMENTOR_PASSWORD", "secret-password");
        if let Some(value) = debug { command.env("INFOMENTOR_DEBUG", value); }
        let output = command.output().unwrap();
        serving.join().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("secret-"), "{stderr}");
        assert!(!stderr.contains('?'), "{stderr}");
        assert!(stderr.ends_with("InfoMentor returned an error. Try again later.\n"));
        if debug == Some("1") {
            assert_eq!(stderr.matches("[infomentor-debug] GET").count(), 2);
            assert!(stderr.contains("status=302"));
            assert!(stderr.contains("status=503"));
            assert!(stderr.contains("location=Some(\"https://minn.infomentor.is/error\")"));
            assert!(stderr.contains("sent_cookies=[\"transaction\"]"));
            assert!(stderr.contains("set_cookies=[\"response\"]"));
            assert_eq!(stderr.matches("mode=browser").count(), 2);
            assert!(stderr.contains("error_body=\"<html><h1>Service Unavailable</h1><p>[redacted]</p></html>\""));
        } else {
            assert_eq!(stderr, "InfoMentor returned an error. Try again later.\n");
        }
    }
}

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        let name = format!(
            "infomentor-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        );
        let path = std::env::temp_dir().join(name);
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn command(&self, origin: Option<&str>, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_infomentor-mcp"));
        command
            .args(args)
            .env_clear()
            .env("HOME", &self.0)
            .env("XDG_CONFIG_HOME", self.0.join(".config"))
            .env("XDG_DATA_HOME", self.0.join(".local/share"))
            .env("FAMILY_MCP_STORE_TEST_SEAM", "1")
            .stdin(Stdio::null());

        if let Some(origin) = origin {
            command.env("INFOMENTOR_TEST_ORIGIN", origin);
        }
        command
    }

    fn run(&self, origin: Option<&str>, args: &[&str]) -> Output {
        self.command(origin, args).output().unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_version_is_the_typescript_package_version() {
    let manifest = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/infomentor-mcp/package.json"),
    )
    .unwrap();
    let package: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    assert_eq!(package["version"], env!("CARGO_PKG_VERSION"));

    let output = Home::new().run(None, &["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn the_test_build_refuses_to_start_without_a_local_upstream() {
    let home = Home::new();

    for origin in [
        None,
        Some("https://minn.infomentor.is"),
        Some("http://localhost:9"),
        // The prefix of a local origin, then user info: the host is example.invalid.
        Some("http://127.0.0.1:1@example.invalid"),
    ] {
        let output = home.run(origin, &["status"]);
        assert!(!output.status.success(), "{origin:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("INFOMENTOR_TEST_ORIGIN must be local."),
            "{origin:?}"
        );
    }
    // Nothing was created: the refusal comes before the store is touched.
    assert_eq!(fs::read_dir(&home.0).unwrap().count(), 0);
}

#[test]
fn requests_go_through_the_proxy_in_http_proxy() {
    let home = Home::new();
    let session = home.0.join("session.json");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&session)
        .unwrap()
        .write_all(br#"{"version":2,"savedAt":"2026-09-01T00:00:00.000Z","cookies":[{"key":"IMHome","value":"synthetic","domain":"minn.infomentor.is","path":"/","secure":true,"httpOnly":true,"hostOnly":true,"creation":"2026-09-01T00:00:00.000Z","lastAccessed":"2026-09-01T00:00:00.000Z"}]}"#)
        .unwrap();
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = proxy.local_addr().unwrap();
    let seen = std::thread::spawn(move || {
        let (stream, _) = proxy.accept().unwrap();
        let mut request = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        request.read_line(&mut line).unwrap();
        (&stream)
            .write_all(
                b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        line
    });
    // The test origin is a closed port: only the proxy can answer.
    let output = home
        .command(
            Some("http://127.0.0.1:9"),
            &["status", "--session", "session.json"],
        )
        .current_dir(&home.0)
        .env("HTTP_PROXY", format!("http://{address}"))
        .output()
        .unwrap();
    assert_eq!(
        seen.join().unwrap(),
        "GET http://127.0.0.1:9/minn.infomentor.is/ HTTP/1.1\r\n"
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "InfoMentor returned an error. Try again later.\n"
    );
}

#[test]
fn a_relative_path_without_a_working_directory_fails_safely() {
    let home = Home::new();
    let gone = home.0.join("gone");
    DirBuilder::new().mode(0o700).create(&gone).unwrap();

    // The shell enters `gone` and removes it, so the binary starts without a working directory.
    let run = |session: &str| {
        let script = r#"cd "$1" && rmdir "$1" && exec "$2" status --session "$3""#;
        // The binary's own environment, given to the shell that starts it.
        let binary = home.command(Some("http://127.0.0.1:9"), &[]);
        let output = Command::new("/bin/sh")
            .args(["-c", script, "sh"])
            .arg(&gone)
            .arg(binary.get_program())
            .arg(session)
            .env_clear()
            .envs(
                binary
                    .get_envs()
                    .filter_map(|(key, value)| Some((key, value?))),
            )
            .stdin(Stdio::null())
            .output()
            .unwrap();
        DirBuilder::new().mode(0o700).create(&gone).unwrap();
        output
    };

    // Resolved from `/` it would name /session.json; it fails as `process.cwd()` throws instead.
    let relative = run("session.json");
    assert_eq!(relative.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&relative.stderr),
        "InfoMentor operation failed. Check the network and session-store permissions.\n"
    );

    // An absolute path needs no working directory, as with `path.resolve`.
    let absolute = run(home.0.join("session.json").to_str().unwrap());
    assert_eq!(absolute.status.code(), Some(1));
    assert!(
        !String::from_utf8_lossy(&absolute.stderr).contains("operation failed"),
        "{absolute:?}"
    );
}
