//! The lock cases of packages/session-store/test/lock.test.ts that tests/files_lock.rs does not
//! already make: waiting, a waiter's temporary under a holder's sweep, a replaced owner, and work
//! that never runs on a hard-linked target, and concurrent recoveries of one dead owner.

mod common;

use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use family_store::{Cancel, Code, Error, LockOptions, sweep_temp, with_file_lock};

fn waiting(wait: Duration) -> LockOptions {
    LockOptions {
        wait,
        ..LockOptions::default()
    }
}

/// Set a file's or folder's modification time ten minutes back, as utimes does.
fn age(path: &std::path::Path) {
    let old = SystemTime::now() - Duration::from_secs(600);
    fs::File::open(path).unwrap().set_modified(old).unwrap();
}

/// The lock temporary a waiter publishes in `directory` once it is inside its lock call (bounded
/// wait). Only one waiter at a time may be polling.
fn published(directory: &std::path::Path) -> std::path::PathBuf {
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        let found = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("session.json.lock-tmp.")
            });

        if let Some(path) = found {
            return path;
        }
        assert!(
            Instant::now() < deadline,
            "no waiter published its lock temporary"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn waiters_poll_a_busy_lock_and_a_live_pid_never_expires_by_age() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    let lock = scratch.join("session.json.lock");
    let (entered, holding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();

    let holder = {
        let file = file.clone();
        std::thread::spawn(move || {
            with_file_lock(&file, &LockOptions::default(), || {
                entered.send(()).unwrap();
                released.recv().unwrap();
                Ok::<_, Error>("held")
            })
        })
    };
    holding.recv().unwrap();

    let immediate = Instant::now();
    assert_eq!(
        code(with_file_lock(&file, &waiting(Duration::ZERO), || Ok::<
            _,
            Error,
        >(
            ()
        ))),
        Some(Code::Busy)
    );
    assert!(immediate.elapsed() < Duration::from_secs(1));

    let bounded = Instant::now();
    assert_eq!(
        code(with_file_lock(
            &file,
            &waiting(Duration::from_millis(200)),
            || { Ok::<_, Error>(()) }
        )),
        Some(Code::Busy)
    );
    let waited = bounded.elapsed();
    assert!(waited >= Duration::from_millis(150), "{waited:?}");
    assert!(waited < Duration::from_secs(3), "{waited:?}");

    // A cancel while waiting ends the wait. The waiter is inside its lock call, polling, once its
    // temporary is published while the holder still holds; it measures its own wait.
    let cancel = Cancel::default();
    let cancelled = {
        let (file, cancel) = (file.clone(), cancel.clone());
        std::thread::spawn(move || {
            let options = LockOptions {
                cancel,
                wait: Duration::from_secs(10),
            };
            let started = Instant::now();
            let outcome = code(with_file_lock(&file, &options, || Ok::<_, Error>(())));
            (outcome, started.elapsed())
        })
    };
    let temporary = published(&scratch.0);
    std::thread::sleep(Duration::from_millis(100));
    assert!(temporary.exists(), "the waiter is still polling");
    cancel.cancel();
    let (outcome, waited) = cancelled.join().unwrap();
    assert_eq!(outcome, Some(Code::Cancelled));
    assert!(waited >= Duration::from_millis(100), "{waited:?}");
    assert!(
        !temporary.exists(),
        "a cancelled waiter removes its temporary"
    );

    // A waiter gets the lock once the holder releases it, and not before: measured inside the
    // waiter, from its lock call to its work.
    let waiter = {
        let file = file.clone();
        std::thread::spawn(move || {
            let started = Instant::now();
            with_file_lock(&file, &waiting(Duration::from_secs(10)), || {
                Ok::<_, Error>(started.elapsed())
            })
        })
    };
    let temporary = published(&scratch.0);
    std::thread::sleep(Duration::from_millis(300));
    assert!(temporary.exists(), "the waiter is still polling");
    release.send(()).unwrap();
    assert_eq!(holder.join().unwrap().unwrap(), "held");
    let waited = waiter.join().unwrap().unwrap();
    assert!(waited >= Duration::from_millis(250), "{waited:?}");
    assert!(waited < Duration::from_secs(10), "{waited:?}");
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);

    // A live PID stays busy however old its owner file is.
    let owner = format!(
        "{}-0b0e5cbb-7d1c-4d7a-9d0e-3f2a6c1b8e4f",
        std::process::id()
    );
    fs::create_dir(&lock).unwrap();
    fs::write(lock.join(&owner), "").unwrap();
    age(&lock.join(&owner));
    assert_eq!(
        code(with_file_lock(&file, &waiting(Duration::ZERO), || Ok::<
            _,
            Error,
        >(
            ()
        ))),
        Some(Code::Busy)
    );
    let left: Vec<_> = fs::read_dir(&lock)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from(owner)]);
}

#[test]
fn a_holders_sweep_leaves_an_old_live_waiters_lock_temporary_alone() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    let (entered, holding) = mpsc::channel();
    let (sweep_now, sweep) = mpsc::channel::<()>();
    let (swept, sweeping) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();

    let holder = {
        let file = file.clone();
        std::thread::spawn(move || {
            with_file_lock(&file, &LockOptions::default(), || {
                entered.send(()).unwrap();
                sweep.recv().unwrap();
                swept
                    .send(sweep_temp(&file, Duration::from_secs(300)))
                    .unwrap();
                released.recv().unwrap();
                Ok::<_, Error>(())
            })
        })
    };
    holding.recv().unwrap();
    let waiter = {
        let file = file.clone();
        std::thread::spawn(move || {
            with_file_lock(&file, &waiting(Duration::from_secs(10)), || {
                Ok::<_, Error>("waited")
            })
        })
    };

    // The waiter publishes its temporary before it polls.
    let mut temporary = None;

    for _ in 0..100 {
        temporary = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("session.json.lock-tmp.")
            });

        if temporary.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let temporary = temporary.expect("the waiter's lock temporary");
    age(&temporary);
    sweep_now.send(()).unwrap();
    assert_eq!(sweeping.recv().unwrap().unwrap(), 0);
    assert!(temporary.exists());

    release.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert_eq!(waiter.join().unwrap().unwrap(), "waited");
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}

#[test]
fn in_flight_lock_loss_is_reported_without_deleting_the_replacement_owner() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    let lock = scratch.join("session.json.lock");
    let replacement = format!(
        "{}-1b4e28ba-2fa1-41d2-883f-0016d3cca427",
        std::process::id()
    );

    let lost = with_file_lock(&file, &waiting(Duration::ZERO), || {
        assert_eq!(fs::read_dir(&lock).unwrap().count(), 1);
        fs::rename(&lock, scratch.join("session.json.lock.previous")).unwrap();
        fs::create_dir(&lock).unwrap();
        fs::write(lock.join(&replacement), "").unwrap();
        Ok::<_, Error>(())
    });
    assert_eq!(code(lost), Some(Code::LockLost));
    let left: Vec<_> = fs::read_dir(&lock)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, [std::ffi::OsString::from(replacement)]);
}

#[test]
fn hard_linked_targets_are_refused_before_any_work() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    write(&file, "private session");
    fs::hard_link(&file, scratch.join("alias.json")).unwrap();
    let mut ran = false;

    let refused = with_file_lock(&file, &waiting(Duration::ZERO), || {
        ran = true;
        Ok::<_, Error>(())
    });
    assert_eq!(code(refused), Some(Code::UnsafeFile));
    assert!(!ran);
    let mut left: Vec<_> = fs::read_dir(&scratch.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(left, ["alias.json", "session.json"]);
}

#[test]
fn concurrent_dead_owner_recoveries_let_exactly_one_in_and_keep_its_owner() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    let lock = scratch.join("session.json.lock");
    let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    // One round seldom lands a second recovery between another's read and its removal; forty do.
    for _ in 0..40 {
        fs::create_dir(&lock).unwrap();
        fs::write(
            lock.join(format!("{dead}-0b0e5cbb-7d1c-4d7a-9d0e-3f2a6c1b8e4f")),
            "",
        )
        .unwrap();

        let start = Arc::new(Barrier::new(12));
        let entered = Arc::new(AtomicUsize::new(0));
        let (release, released) = mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let (outcome, outcomes) = mpsc::channel();
        let attempts: Vec<_> = (0..12)
            .map(|_| {
                let (file, lock, start, entered, released, outcome) = (
                    file.clone(),
                    lock.clone(),
                    start.clone(),
                    entered.clone(),
                    released.clone(),
                    outcome.clone(),
                );
                std::thread::spawn(move || {
                    start.wait();
                    let result = with_file_lock(&file, &waiting(Duration::ZERO), || {
                        entered.fetch_add(1, Ordering::SeqCst);
                        // The winner's owner is the only one, and no other recovery removed it.
                        let owners: Vec<_> = fs::read_dir(&lock)
                            .unwrap()
                            .map(|entry| entry.unwrap().path())
                            .collect();
                        assert_eq!(owners.len(), 1);
                        assert_eq!(mode(&lock), 0o700);
                        assert_eq!(mode(&owners[0]), 0o600);
                        released.lock().unwrap().recv().unwrap();
                        assert_eq!(
                            code(with_file_lock(&file, &waiting(Duration::ZERO), || Ok::<
                                _,
                                Error,
                            >(
                                ()
                            ))),
                            Some(Code::Busy)
                        );
                        Ok::<_, Error>("collected")
                    });
                    outcome.send(result).unwrap();
                })
            })
            .collect();

        // Every attempt but the one that entered gives up as BUSY while it holds the lock.
        for _ in 0..11 {
            let result = outcomes.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(code(result), Some(Code::Busy));
        }
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        release.send(()).unwrap();
        assert_eq!(
            outcomes
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .unwrap(),
            "collected"
        );
        for attempt in attempts {
            attempt.join().unwrap();
        }
        assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
    }

    // Work's own error passes through unchanged even when the caller cancelled meanwhile.
    let cancel = Cancel::default();
    let options = LockOptions {
        cancel: cancel.clone(),
        wait: Duration::ZERO,
    };
    let failed = with_file_lock(&file, &options, || {
        cancel.cancel();
        Err::<(), _>(Error::new(Code::NotFound, "could not confirm restoration"))
    })
    .unwrap_err();
    assert_eq!(
        (failed.code, failed.message),
        (Code::NotFound, "could not confirm restoration")
    );
    assert_eq!(fs::read_dir(&scratch.0).unwrap().count(), 0);
}
