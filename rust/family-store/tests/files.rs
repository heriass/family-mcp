//! The private-file cases of packages/session-store/test/files.test.ts that
//! tests/files_lock.rs does not already make, and the sweep of what is not this file's own temporary.

mod common;

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};

use common::*;
use std::time::{Duration, SystemTime};

use family_store::{
    Cancel, Code, DEFAULT_SWEEP_AGE, ensure_private_dir, read_private_file, sweep_temp,
    write_private_file,
};

#[test]
fn private_files_are_written_atomically_owner_only_and_read_back() {
    let scratch = Scratch::new();
    let nested = scratch.join("nested");
    let file = nested.join("session.json");

    write(&file, "{\"version\":1}\n");
    assert_eq!(read_private_file(&file, 1024).unwrap(), "{\"version\":1}\n");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&nested), 0o700);

    // A replacement is whole, and no temporary is left beside it.
    write_private_file(&file, b"replaced", &Cancel::default()).unwrap();
    assert_eq!(read_private_file(&file, 8).unwrap(), "replaced");
    assert_eq!(code(read_private_file(&file, 7)), Some(Code::TooLarge));
    let names: Vec<_> = fs::read_dir(&nested)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["session.json"]);
    assert_eq!(
        code(read_private_file(&scratch.join("missing.json"), 8)),
        Some(Code::NotFound)
    );
    assert_eq!(code(read_private_file(&nested, 8)), Some(Code::UnsafeFile));

    // The refusals say what to do.
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    let open = read_private_file(&file, 8).unwrap_err();
    assert_eq!(open.code, Code::UnsafeFile);
    assert!(open.message.contains("chmod 600"), "{}", open.message);
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let link = nested.join("link.json");
    symlink(&file, &link).unwrap();
    let linked = read_private_file(&link, 8).unwrap_err();
    assert_eq!(linked.code, Code::UnsafeFile);
    assert!(
        linked.message.contains("link to another file"),
        "{}",
        linked.message
    );

    // A symbolic link at the destination is replaced by the rename, never followed.
    write_private_file(&link, b"unlinked", &Cancel::default()).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().len(), 8);
    assert_eq!(text(&file), "replaced");
    assert_eq!(read_private_file(&link, 8).unwrap(), "unlinked");
    assert!(
        !fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn private_directories_are_created_0700_and_tightened_only_on_request() {
    let scratch = Scratch::new();
    let a = scratch.join("a");

    ensure_private_dir(&a.join("b"), false).unwrap();
    assert_eq!(mode(&a.join("b")), 0o700);
    fs::set_permissions(&a, fs::Permissions::from_mode(0o755)).unwrap();
    ensure_private_dir(&a, false).unwrap();
    assert_eq!(mode(&a), 0o755);
    ensure_private_dir(&a, true).unwrap();
    assert_eq!(mode(&a), 0o700);

    // A file in the way is an error, and stays a file.
    fs::write(scratch.join("file"), "").unwrap();
    assert!(ensure_private_dir(&scratch.join("file"), false).is_err());
    assert!(fs::metadata(scratch.join("file")).unwrap().is_file());
}

#[test]
fn sweeps_leave_planted_directories_pending_files_and_lock_temporaries_alone() {
    let scratch = Scratch::new();
    let file = scratch.join("session.json");
    write(&file, "live");
    let id = |n: u32| format!("0b0e5cbb-7d1c-4d7a-9d0e-{n:012x}");
    let stale = scratch.join(&format!("session.json.{}.tmp", id(1)));
    let fresh = scratch.join(&format!("session.json.{}.tmp", id(2)));
    let pending = scratch.join(&format!("session.json.{}.pending", id(3)));
    let lock_temporary = scratch.join(&format!(
        "session.json.lock.{}-{}.tmp",
        std::process::id(),
        id(4)
    ));
    let loose = scratch.join("session.json.0123.tmp");
    let unrelated = scratch.join(&format!("other.json.{}.tmp", id(5)));
    // Someone else's data under a temporary's exact name is a directory, never a sweep target.
    let planted = scratch.join(&format!("session.json.{}.tmp", id(6)));
    for path in [&stale, &fresh, &pending, &loose, &unrelated] {
        write(path, "");
    }
    fs::create_dir(&lock_temporary).unwrap();
    write(&lock_temporary.join("owner"), "");
    write(&planted.join("checkouts/quote.json"), "kept");
    let old = SystemTime::now() - Duration::from_secs(600);

    for path in [
        &stale,
        &pending,
        &lock_temporary,
        &loose,
        &unrelated,
        &planted,
    ] {
        fs::File::open(path).unwrap().set_modified(old).unwrap();
    }
    let listed = || {
        let mut names: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        names.sort();
        names
    };
    let mut kept = vec![
        file.clone(),
        fresh.clone(),
        pending.clone(),
        lock_temporary.clone(),
        loose.clone(),
        unrelated.clone(),
        planted.clone(),
    ];
    kept.sort();

    assert_eq!(sweep_temp(&file, DEFAULT_SWEEP_AGE).unwrap(), 1);
    assert_eq!(listed(), kept);
    assert_eq!(sweep_temp(&file, DEFAULT_SWEEP_AGE).unwrap(), 0);
    fs::File::open(&fresh).unwrap().set_modified(old).unwrap();
    assert_eq!(sweep_temp(&file, Duration::ZERO).unwrap(), 1);
    kept.retain(|path| path != &fresh);
    assert_eq!(listed(), kept);
    assert_eq!(text(&planted.join("checkouts/quote.json")), "kept");
    assert_eq!(
        sweep_temp(&scratch.join("absent/session.json"), Duration::ZERO).unwrap(),
        0
    );
}
