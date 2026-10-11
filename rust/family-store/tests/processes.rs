//! Several processes on one store or locked file: mutual exclusion, serialized read-modify-write
//! cycles, and takeover of a lock whose holder was killed. Each other process is this test binary
//! running the ignored `worker` case. In place of the cross-language lock cases of
//! tests/interop.rs and of packages/session-store/test/{processes,secret}.test.ts.

mod common;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use family_store::{
    Cancel, Code, Error, LocalKeyFileProvider, LockOptions, SecretRecordOptions, create_secret_key,
    read_private_file, read_secret_record, with_file_lock, with_secret_record, with_secret_store,
    write_private_file,
};

const MODE: &str = "FAMILY_STORE_TEST_WORKER_MODE";

const TARGET: &str = "FAMILY_STORE_TEST_WORKER_TARGET";

const ARGUMENT: &str = "FAMILY_STORE_TEST_WORKER_ARGUMENT";

/// The record of tests/common `options_with` under `root`, with its key in a file.
fn store_in(root: &Path) -> SecretRecordOptions {
    options_with(
        root,
        Arc::new(LocalKeyFileProvider::new(
            root.join("keys").join("test-mcp.default.key"),
        )),
        1024,
    )
}

fn pause(argument: &str) -> Duration {
    Duration::from_millis(argument.parse().unwrap())
}

fn counter(file: &Path) -> u32 {
    match read_private_file(file, 64) {
        Ok(text) => text.parse().unwrap(),
        Err(error) if error.code == Code::NotFound => 0,
        Err(error) => panic!("{error:?}"),
    }
}

fn wait_for(path: &Path) {
    let end = Instant::now() + Duration::from_secs(60);

    while !path.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One operation in a separate process, as packages/session-store/test/*-worker.ts did. Prints
/// progress lines and `result:<outcome>`; a store error is an outcome, anything else a failure.
#[test]
#[ignore = "run in a child process by the tests of this file"]
fn worker() {
    let mode = std::env::var(MODE).unwrap();
    let target = PathBuf::from(std::env::var_os(TARGET).unwrap());
    let argument = std::env::var(ARGUMENT).unwrap_or_default();
    let store = store_in(&target);
    let no_wait = SecretRecordOptions {
        wait: Duration::ZERO,
        ..store.clone()
    };
    let say = |line: &str| println!("{line}");

    let outcome: Result<Option<String>, Error> = match mode.as_str() {
        // Read, pause, write: without mutual exclusion concurrent runs lose updates.
        "increment-record" => with_secret_record(&store, |current| {
            std::thread::sleep(pause(&argument));
            let count: u32 = current.unwrap_or("0").parse().unwrap();
            Ok::<_, Error>(Some((count + 1).to_string()))
        }),
        // Two writes in one hold with a pause between them; `written` lets a test kill it there.
        "two-writes" => with_secret_store(&store, |held| {
            let current: u32 = held.read()?.as_deref().unwrap_or("0").parse().unwrap();
            held.write(&(current + 1).to_string())?;
            say("written");
            std::thread::sleep(pause(&argument));
            held.write(&(current + 2).to_string())?;
            Ok(None)
        }),
        // Hold the record lock until the release file appears.
        "hold-record" => with_secret_store(&store, |_| {
            say("held");
            wait_for(Path::new(&argument));
            Ok(None)
        }),
        "put-no-wait" => with_secret_record(&no_wait, |_| Ok::<_, Error>(Some(argument.clone()))),
        "read-no-wait" => read_secret_record(&no_wait).map(Some),
        // The same three on a plain private file and its lock.
        "increment-file" => with_file_lock(&target, &LockOptions::default(), || {
            let value = counter(&target);
            std::thread::sleep(pause(&argument));
            write_private_file(
                &target,
                (value + 1).to_string().as_bytes(),
                &Cancel::default(),
            )?;
            Ok::<_, Error>(None)
        }),
        "hold-file" => with_file_lock(&target, &LockOptions::default(), || {
            say("held");
            wait_for(Path::new(&argument));
            Ok::<_, Error>(None)
        }),
        "remove-file" => {
            say("started");
            let started = Instant::now();
            with_file_lock(&target, &LockOptions::default(), || {
                // Measured here, from the lock call to the work.
                say(&format!("waited:{}", started.elapsed().as_millis()));
                fs::remove_file(&target).unwrap();
                Ok::<_, Error>(None)
            })
        }
        other => panic!("unknown worker mode {other}"),
    };
    let shown = match outcome {
        Ok(Some(value)) => format!("value:{value}"),
        Ok(None) => "none".to_owned(),
        Err(error) => format!("error:{}", error.code.as_str()),
    };
    println!("result:{shown}");
}

/// Start the worker in `mode` on `target` (a store root, or a plain file for the file modes).
fn start(scratch: &Scratch, mode: &str, target: &Path, argument: &str) -> Child {
    child_case("worker", &scratch.0)
        .env(MODE, mode)
        .env(TARGET, target)
        .env(ARGUMENT, argument)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Read the worker's stdout until `line`.
fn printed(child: &mut Child, line: &str) {
    let mut reader = BufReader::new(child.stdout.as_mut().unwrap());
    let mut seen = String::new();

    loop {
        seen.clear();
        assert_ne!(
            reader.read_line(&mut seen).unwrap(),
            0,
            "the worker ended before {line}"
        );

        if seen.trim_end() == line {
            return;
        }
    }
}

/// The worker's stdout once it exits successfully.
fn output(child: Child) -> String {
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// The value of the worker's `<prefix>` line.
fn line(stdout: &str, prefix: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap_or_else(|| panic!("no {prefix} {stdout}"))
        .to_owned()
}

/// The worker's `result:` line once it exits successfully.
fn finished(child: Child) -> String {
    line(&output(child), "result:")
}

fn run(scratch: &Scratch, mode: &str, target: &Path, argument: &str) -> String {
    finished(start(scratch, mode, target, argument))
}

fn read(store: &SecretRecordOptions) -> String {
    match read_secret_record(store) {
        Ok(value) => format!("value:{value}"),
        Err(error) => format!("error:{}", error.code.as_str()),
    }
}

#[test]
fn a_lock_held_by_one_process_blocks_another() {
    let scratch = Scratch::new();
    let store = store_in(&scratch.0);
    create_secret_key(&store).unwrap();

    // This process holds: another one is BUSY at once and after a bounded wait.
    with_secret_store(&store, |held| {
        held.write("held-here")?;
        assert_eq!(
            run(&scratch, "put-no-wait", &scratch.0, "intruder"),
            "error:BUSY"
        );
        assert_eq!(run(&scratch, "read-no-wait", &scratch.0, ""), "error:BUSY");
        Ok::<_, Error>(())
    })
    .unwrap();
    assert_eq!(read(&store), "value:held-here");

    // Another process holds: this one is BUSY, then proceeds once the hold ends.
    let release = scratch.join("release");
    let mut holder = start(
        &scratch,
        "hold-record",
        &scratch.0,
        release.to_str().unwrap(),
    );
    printed(&mut holder, "held");
    let no_wait = SecretRecordOptions {
        wait: Duration::ZERO,
        ..store.clone()
    };
    assert_eq!(code(put(&no_wait, "intruder")), Some(Code::Busy));
    let brief = SecretRecordOptions {
        wait: Duration::from_millis(300),
        ..store.clone()
    };
    let started = Instant::now();
    assert_eq!(code(put(&brief, "intruder")), Some(Code::Busy));
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert_eq!(read(&no_wait), "error:BUSY");
    fs::write(&release, "").unwrap();
    assert_eq!(finished(holder), "none");
    assert_eq!(put(&no_wait, "after").unwrap().as_deref(), Some("after"));
    assert_eq!(run(&scratch, "read-no-wait", &scratch.0, ""), "value:after");
}

#[test]
fn separate_processes_serialize_read_modify_write_cycles_on_one_record() {
    let scratch = Scratch::new();
    let store = store_in(&scratch.0);
    create_secret_key(&store).unwrap();

    let workers: Vec<Child> = (0..3)
        .map(|_| start(&scratch, "increment-record", &scratch.0, "100"))
        .collect();
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let store = store.clone();
            std::thread::spawn(move || {
                with_secret_record(&store, |current| {
                    std::thread::sleep(Duration::from_millis(100));
                    let count: u32 = current.unwrap_or("0").parse().unwrap();
                    Ok::<_, Error>(Some((count + 1).to_string()))
                })
            })
        })
        .collect();

    for thread in threads {
        thread.join().unwrap().unwrap();
    }
    let mut results: Vec<String> = workers.into_iter().map(finished).collect();
    results.sort();
    // Six increments, each a committed generation: none was lost to another process.
    assert_eq!(read(&store), "value:6");
    assert!(text(&marker_path(&store)).ends_with("\"generation\":6}\n"));
    assert!(results.iter().all(|result| result.starts_with("value:")));
    assert!(!store.path.with_extension("enc.lock").exists());
}

#[test]
fn separate_processes_serialize_multi_write_holds_and_a_crash_keeps_the_last_commit() {
    let scratch = Scratch::new();
    let store = store_in(&scratch.0);
    create_secret_key(&store).unwrap();

    let workers: Vec<Child> = (0..3)
        .map(|_| start(&scratch, "two-writes", &scratch.0, "150"))
        .collect();

    for worker in workers {
        assert_eq!(finished(worker), "none");
    }
    assert_eq!(read(&store), "value:6");
    assert!(text(&marker_path(&store)).ends_with("\"generation\":6}\n"));

    // Killed between its two writes while holding the lock: the first write stays committed.
    let mut worker = start(&scratch, "two-writes", &scratch.0, "60000");
    printed(&mut worker, "written");
    worker.kill().unwrap();
    worker.wait().unwrap();
    assert_eq!(read(&store), "value:7");
    assert!(text(&marker_path(&store)).ends_with("\"generation\":7}\n"));
}

#[test]
fn a_lock_left_by_a_killed_holder_is_recovered() {
    let scratch = Scratch::new();
    let store = store_in(&scratch.0);
    create_secret_key(&store).unwrap();
    let lock = store.path.with_extension("enc.lock");
    let no_wait = SecretRecordOptions {
        wait: Duration::ZERO,
        ..store.clone()
    };

    // A holder dies: its owner file stays, and this process takes the lock over.
    let never = scratch.join("never");
    let mut holder = start(&scratch, "hold-record", &scratch.0, never.to_str().unwrap());
    printed(&mut holder, "held");
    assert_eq!(code(put(&no_wait, "intruder")), Some(Code::Busy));
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(fs::read_dir(&lock).unwrap().count(), 1);
    assert_eq!(
        put(&no_wait, "after-crash").unwrap().as_deref(),
        Some("after-crash")
    );
    assert!(!lock.exists());

    // Another holder dies: a third process takes the lock over.
    let mut holder = start(&scratch, "hold-record", &scratch.0, never.to_str().unwrap());
    printed(&mut holder, "held");
    assert_eq!(
        run(&scratch, "put-no-wait", &scratch.0, "intruder"),
        "error:BUSY"
    );
    holder.kill().unwrap();
    holder.wait().unwrap();
    assert_eq!(fs::read_dir(&lock).unwrap().count(), 1);
    assert_eq!(
        run(&scratch, "put-no-wait", &scratch.0, "after-second-crash"),
        "value:after-second-crash"
    );
    assert!(!lock.exists());
    assert_eq!(read(&store), "value:after-second-crash");
}

#[test]
fn separate_processes_serialize_file_locks_and_removal_waits_for_an_in_flight_holder() {
    let scratch = Scratch::new();
    let file = scratch.join("data").join("session.json");
    write(&file, "0");

    let workers: Vec<Child> = (0..3)
        .map(|_| start(&scratch, "increment-file", &file, "150"))
        .collect();

    for worker in workers {
        assert_eq!(finished(worker), "none");
    }
    assert_eq!(text(&file), "3");
    assert_eq!(
        fs::read_dir(scratch.join("data")).unwrap().count(),
        1,
        "no lock or temporary is left"
    );

    let release = scratch.join("release");
    let mut holder = start(&scratch, "hold-file", &file, release.to_str().unwrap());
    printed(&mut holder, "held");
    let mut remover = start(&scratch, "remove-file", &file, "");
    printed(&mut remover, "started");
    // The remover is inside its lock call once its own lock temporary is published.
    let temporary = format!("session.json.lock-tmp.{}-", remover.id());
    let deadline = Instant::now() + Duration::from_secs(60);

    while !fs::read_dir(scratch.join("data")).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(&temporary)
    }) {
        assert!(
            Instant::now() < deadline,
            "the remover never started waiting"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(text(&file), "3");
    assert!(remover.try_wait().unwrap().is_none(), "the remover waits");
    fs::write(&release, "").unwrap();
    assert_eq!(finished(holder), "none");
    let removed = output(remover);
    assert_eq!(line(&removed, "result:"), "none");
    let waited: u64 = line(&removed, "waited:").parse().unwrap();
    assert!(waited >= 250, "{waited} ms");
    assert_eq!(fs::read_dir(scratch.join("data")).unwrap().count(), 0);
}
