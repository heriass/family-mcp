use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use rustix::fs::OFlags;
use rustix::io::Errno;

use crate::errors::{Cancel, Code, Error, Result, errno};

pub const DEFAULT_SWEEP_AGE: Duration = Duration::from_secs(300);

pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|cause| {
        Error::caused(
            Code::Io,
            "Cannot read random bytes from the system.",
            cause.to_string(),
        )
    })?;
    Ok(bytes)
}

/// A random version-4 UUID in the form Node's `randomUUID` prints.
pub(crate) fn uuid() -> Result<String> {
    let mut bytes = random::<16>()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

pub(crate) fn is_uuid(text: &str) -> bool {
    text.len() == 36
        && text.bytes().enumerate().all(|(at, byte)| match at {
            8 | 13 | 18 | 23 => byte == b'-',
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `<path><suffix>`, as the TypeScript package builds marker, lock and temporary names.
pub(crate) fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path);
    name.push(suffix);
    name.into()
}

/// Node's `normalize`: `.` and `..` resolved lexically, repeated and trailing separators dropped.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut normal = PathBuf::new();

    for component in path.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other),
        }
    }
    normal
}

/// Node's `resolve`: absolute against the working directory, then normalized.
pub(crate) fn resolve(path: &Path) -> PathBuf {
    normalize(&std::env::current_dir().unwrap_or_default().join(path))
}

/// Node's `dirname`: a bare file name lives in `.`.
pub(crate) fn parent(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        Some(_) => Path::new("."),
        None => path,
    }
}

// Shared with the store's startup check, which prints the path and any command beside them.
pub(crate) const LINKED_FILE: &str =
    "This file has a second name (a hard link). Remove the other name and start again.";

pub(crate) const OPEN_FILE: &str = "Other users can open this file.";

// OPEN_FILE and its command, for readers that print no path.
const OPEN_FILE_FIX: &str = "Other users can open this file. Make it owner-only with chmod 600.";

pub(crate) const FOREIGN_FILE: &str =
    "This file belongs to another user, often root after a sudo run.";

pub(crate) const LINK_FILE: &str =
    "This file is a link to another file. Put the real file here and start again.";

pub(crate) const NOT_A_FILE: &str = "Something other than a plain file is at this path.";

pub(crate) fn assert_no_hard_links(nlink: u64) -> Result<()> {
    if nlink > 1 {
        return Err(Error::new(Code::UnsafeFile, LINKED_FILE));
    }
    Ok(())
}

/// Read a file that must be a regular, single-link, owner-only file owned by this user. Bytes
/// that are not UTF-8 decode to U+FFFD, as in the TypeScript package.
pub fn read_private_file(path: &Path, max_bytes: usize) -> Result<String> {
    Ok(String::from_utf8_lossy(&read_private_bytes(path, max_bytes)?).into_owned())
}

/// [`read_private_file`] without text decoding, for binary files such as keys.
pub fn read_private_bytes(path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
    // O_NOFOLLOW refuses symbolic links and O_NONBLOCK keeps a FIFO from blocking the open.
    let flags = (OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32;
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(flags)
        .open(path)
        .map_err(|error| match errno(&error) {
            Some(Errno::NOENT) => Error::caused(Code::NotFound, "The file does not exist.", error),
            Some(Errno::LOOP | Errno::MLINK) => Error::caused(Code::UnsafeFile, LINK_FILE, error),
            Some(Errno::ISDIR) => Error::caused(Code::UnsafeFile, NOT_A_FILE, error),
            _ => Error::io(
                error,
                "Cannot open the file. Check its path and permissions.",
            ),
        })?;
    let read_failed = |error| Error::io(error, "Cannot read the file. Check its permissions.");
    let info = file.metadata().map_err(read_failed)?;

    if !info.is_file() {
        return Err(Error::new(Code::UnsafeFile, NOT_A_FILE));
    }
    assert_no_hard_links(info.nlink())?;

    if info.mode() & 0o077 != 0 {
        return Err(Error::new(Code::UnsafeFile, OPEN_FILE_FIX));
    }

    if info.uid() != current_uid() {
        return Err(Error::new(Code::UnsafeFile, FOREIGN_FILE));
    }

    if info.size() > max_bytes as u64 {
        return Err(Error::new(
            Code::TooLarge,
            "The file is larger than allowed.",
        ));
    }
    let changed = || Error::new(Code::Io, "The file changed while it was being read.");
    let mut bytes = Vec::with_capacity(info.size() as usize + 1);
    // One extra byte detects a file that grows while it is read.
    (&mut file)
        .take(info.size() + 1)
        .read_to_end(&mut bytes)
        .map_err(read_failed)?;

    if bytes.len() as u64 != info.size() {
        return Err(changed());
    }
    let after = file.metadata().map_err(read_failed)?;

    if (info.ino(), info.size(), info.mtime(), info.mtime_nsec())
        != (after.ino(), after.size(), after.mtime(), after.mtime_nsec())
    {
        return Err(changed());
    }
    Ok(bytes)
}

#[cfg(test)]
thread_local! {
    /// Test only: the uid [`read_private_bytes`] compares owners with on this thread, in place of
    /// getuid, as the TypeScript test faked `process.getuid`.
    static FAKE_UID: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };

    /// Test only: run once by the writer on this thread after its temporary is written and
    /// flushed, just before the commit-point cancel check.
    static BEFORE_COMMIT: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// This process's uid (a test may fake it on its own thread).
fn current_uid() -> u32 {
    #[cfg(test)]
    if let Some(uid) = FAKE_UID.with(std::cell::Cell::get) {
        return uid;
    }
    rustix::process::getuid().as_raw()
}

/// Replace `path` atomically with an owner-only file: an exclusive 0600 temporary is written and
/// flushed, then renamed over the destination, and the directory is flushed as well. A cancel
/// observed before the rename leaves the previous file untouched.
pub fn write_private_file(path: &Path, data: &[u8], cancel: &Cancel) -> Result<()> {
    cancel.check()?;
    let directory = parent(path);
    ensure_private_dir(directory, false)?;
    let temporary = suffixed(path, &format!(".{}.tmp", uuid()?));
    let result = replace(path, &temporary, data, cancel);
    let _ = fs::remove_file(&temporary);
    result?;
    sync_directory(directory);
    Ok(())
}

fn replace(path: &Path, temporary: &Path, data: &[u8], cancel: &Cancel) -> Result<()> {
    let mut file = create_private(temporary).map_err(|error| {
        Error::io(
            error,
            "Cannot create a temporary file. Check the directory permissions.",
        )
    })?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .map_err(|error| {
            Error::io(
                error,
                "Cannot write the file. Check the disk and directory permissions.",
            )
        })?;
    drop(file);
    #[cfg(test)]
    if let Some(hook) = BEFORE_COMMIT.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    // The rename is the commit point; a cancel observed here keeps the previous file.
    cancel.check()?;
    fs::rename(temporary, path).map_err(|error| {
        Error::io(
            error,
            "Cannot replace the file. Check the directory permissions.",
        )
    })
}

/// Node's `open(path, 'wx', 0o600)`.
pub(crate) fn create_private(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Create `path` and missing parents with mode 0700; an existing directory keeps its mode unless
/// `enforce_mode` asks to tighten it.
pub fn ensure_private_dir(path: &Path, enforce_mode: bool) -> Result<()> {
    let failed = |error| Error::io(error, "Cannot create the directory. Check its permissions.");
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(failed)?;
    let info = fs::metadata(path).map_err(failed)?;

    if !info.is_dir() {
        return Err(Error::new(
            Code::UnsafeFile,
            "The directory path is not a directory.",
        ));
    }

    if enforce_mode && info.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(failed)?;
    }
    Ok(())
}

/// Remove orphaned `<name>.<uuid>.tmp` regular files beside `path` that are at least
/// `older_than` old. Returns the count.
pub fn sweep_temp(path: &Path, older_than: Duration) -> Result<usize> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(0);
    };
    let prefix = format!("{name}.");
    let entries = match fs::read_dir(parent(path)) {
        Ok(entries) => entries,
        Err(error) if errno(&error) == Some(Errno::NOENT) => return Ok(0),
        Err(error) => {
            return Err(Error::io(
                error,
                "Cannot list the directory. Check its permissions.",
            ));
        }
    };
    let cutoff = SystemTime::now().checked_sub(older_than);
    let mut removed = 0;

    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::io(error, "Cannot list the directory. Check its permissions.")
        })?;
        let own = entry.file_name().to_str().is_some_and(|name| {
            name.strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .is_some_and(is_uuid)
        });

        if !own {
            continue;
        }
        let info = match fs::symlink_metadata(entry.path()) {
            Ok(info) => info,
            Err(error) if errno(&error) == Some(Errno::NOENT) => continue,
            Err(error) => {
                return Err(Error::io(
                    error,
                    "Cannot inspect a temporary file. Check the directory permissions.",
                ));
            }
        };
        let old = info
            .modified()
            .is_ok_and(|modified| cutoff.is_some_and(|cutoff| modified <= cutoff));

        // A directory or link under a temporary's name was never one; it is left alone.
        if !info.is_file() || !old {
            continue;
        }

        match fs::remove_file(entry.path()) {
            Ok(()) => removed += 1,
            Err(error) if errno(&error) == Some(Errno::NOENT) => removed += 1,
            Err(error) => {
                return Err(Error::io(
                    error,
                    "Cannot remove a temporary file. Check the directory permissions.",
                ));
            }
        }
    }
    Ok(removed)
}

// Directory flushes are best effort: the data file is already durable and renamed, and some
// filesystems refuse to open or sync a directory handle.
pub(crate) fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_observed_at_the_commit_point_keeps_the_previous_file_and_no_temporary() {
        let directory =
            std::env::temp_dir().join(format!("family-store-commit-{}", uuid().unwrap()));
        ensure_private_dir(&directory, false).unwrap();
        let file = directory.join("session.json");
        write_private_file(&file, b"previous", &Cancel::default()).unwrap();
        let cancel = Cancel::default();
        cancel.cancel();
        assert_eq!(
            write_private_file(&file, b"never", &cancel)
                .unwrap_err()
                .code,
            Code::Cancelled
        );

        // `replace` checks only before the rename: the temporary is fully written and flushed,
        // and the cancel is observed immediately before the commit point.
        let temporary = suffixed(&file, &format!(".{}.tmp", uuid().unwrap()));
        let committed = replace(&file, &temporary, b"never", &cancel);
        assert_eq!(committed.unwrap_err().code, Code::Cancelled);
        assert_eq!(fs::read(&temporary).unwrap(), b"never");
        assert_eq!(read_private_file(&file, 64).unwrap(), "previous");

        // The cleanup a cancelled commit relies on: a replacement that fails after its temporary
        // was written (here the rename, onto a folder that is not empty) leaves no temporary.
        fs::remove_file(&temporary).unwrap();
        let occupied = directory.join("occupied");
        fs::create_dir(&occupied).unwrap();
        fs::write(occupied.join("inside"), "").unwrap();
        assert!(write_private_file(&occupied, b"never", &Cancel::default()).is_err());
        fs::remove_file(occupied.join("inside")).unwrap();
        fs::remove_dir(&occupied).unwrap();
        let names: Vec<_> = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["session.json"]);
        assert_eq!(read_private_file(&file, 64).unwrap(), "previous");
        fs::remove_file(&file).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    /// The names in `directory`, sorted.
    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_public_writer_cancelled_at_its_commit_point_keeps_the_previous_bytes_and_no_temporary() {
        let directory =
            std::env::temp_dir().join(format!("family-store-commit-hook-{}", uuid().unwrap()));
        ensure_private_dir(&directory, false).unwrap();
        let file = directory.join("session.json");
        write_private_file(&file, b"previous", &Cancel::default()).unwrap();
        let cancel = Cancel::default();
        let cancelled = cancel.clone();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let (record, watched) = (seen.clone(), directory.clone());
        // The cancel arrives after the writer's initial check, once its temporary is written.
        BEFORE_COMMIT.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                *record.borrow_mut() = names(&watched);
                cancelled.cancel();
            }));
        });

        assert_eq!(
            write_private_file(&file, b"never", &cancel)
                .unwrap_err()
                .code,
            Code::Cancelled
        );
        // The hook ran: the temporary existed beside the file when the cancel came.
        let before = seen.borrow().clone();
        assert_eq!(before.len(), 2, "{before:?}");
        assert!(
            before[1].starts_with("session.json.") && before[1].ends_with(".tmp"),
            "{before:?}"
        );
        // No manual cleanup: the writer itself left exactly the previous file.
        assert_eq!(names(&directory), ["session.json"]);
        assert_eq!(fs::read(&file).unwrap(), b"previous");
        fs::remove_file(&file).unwrap();
        fs::remove_dir(&directory).unwrap();
    }

    #[test]
    fn a_file_owned_by_another_user_is_refused_by_the_reader_itself() {
        let directory =
            std::env::temp_dir().join(format!("family-store-foreign-{}", uuid().unwrap()));
        ensure_private_dir(&directory, false).unwrap();
        let file = directory.join("session.json");
        write_private_file(&file, b"private", &Cancel::default()).unwrap();
        assert_eq!(read_private_file(&file, 1024).unwrap(), "private");

        // As if the file belonged to another user: this thread's uid differs from its owner's.
        let owner = fs::metadata(&file).unwrap().uid();
        FAKE_UID.with(|uid| uid.set(Some(owner + 1)));
        let refused = read_private_file(&file, 1024).unwrap_err();
        let bytes = read_private_bytes(&file, 1024).unwrap_err();
        FAKE_UID.with(|uid| uid.set(None));
        assert_eq!(
            (refused.code, refused.message),
            (Code::UnsafeFile, FOREIGN_FILE)
        );
        assert_eq!(
            (bytes.code, bytes.message),
            (Code::UnsafeFile, FOREIGN_FILE)
        );
        assert_eq!(read_private_file(&file, 1024).unwrap(), "private");
        fs::remove_file(&file).unwrap();
        fs::remove_dir(&directory).unwrap();
    }
}
