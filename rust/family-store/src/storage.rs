use std::collections::{BTreeSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs::{self, DirBuilder, Metadata};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rustix::io::Errno;

use crate::errors::{Code, Error, Result, errno};
use crate::files::{FOREIGN_FILE, LINK_FILE, LINKED_FILE, NOT_A_FILE, OPEN_FILE, parent, resolve};
use crate::seam::{TEST_LS, fake_executable};
use crate::spawn::run_bounded;

const NOT_A_DIRECTORY: &str =
    "Something other than a folder is at this store path. Move it away and start again.";

const LINKED_DIRECTORY: &str =
    "This store folder is a link to another place. Replace it with a real folder and start again.";

const FOREIGN_DIRECTORY: &str =
    "This store folder belongs to another user, often root after a sudo run.";

const OPEN_DIRECTORY: &str = "Other users can open this store folder.";

const WRITABLE_ANCESTOR: &str = "Other users can write to a folder above the store.";

const FOREIGN_ANCESTOR: &str = "A folder above the store belongs to another user.";

const BROKEN_LINK: &str = "A link in a folder above the store is broken, loops back on itself, or leads into a folder you cannot open.";

const OWNED_ACL: &str = "Extra sharing permissions (an access control list, set in Finder’s Get Info) let other users in.";

// `chmod -N` would also drop the stock `everyone deny delete` entry, so this one has no command.
const ANCESTOR_ACL: &str = "Extra sharing permissions (an access control list) on a folder above the store let other users change it. List them with ls -led and remove the entry that allows another user to write.";

const ACL_UNREADABLE: &str =
    "Cannot check the store’s access control lists. Check its permissions and try again.";

/// The command for each refusal that has one; the startup line adds the quoted path.
fn fix(problem: &str) -> Option<&'static str> {
    Some(match problem {
        FOREIGN_DIRECTORY => r#"sudo chown -R "$(id -un)""#,
        OPEN_DIRECTORY => "chmod 700",
        WRITABLE_ANCESTOR => "chmod go-w",
        OPEN_FILE => "chmod 600",
        FOREIGN_FILE => r#"sudo chown "$(id -un)""#,
        OWNED_ACL => "chmod -N",
        _ => return None,
    })
}

/// A refusal with its fixed command, if it has one.
fn refusal(problem: &'static str, path: impl Into<PathBuf>) -> Error {
    Error::refusal(problem, path, fix(problem))
}

const STICKY: u32 = 0o1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Directory,
    File,
    Link,
    Other,
}

/// What the decisions below read of an `lstat`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StoreStat {
    pub kind: Kind,
    pub mode: u32,
    pub uid: u32,
    pub nlink: u64,
}

impl From<&Metadata> for StoreStat {
    fn from(info: &Metadata) -> Self {
        let kind = info.file_type();
        Self {
            kind: if kind.is_symlink() {
                Kind::Link
            } else if kind.is_dir() {
                Kind::Directory
            } else if kind.is_file() {
                Kind::File
            } else {
                Kind::Other
            },
            mode: info.mode(),
            uid: info.uid(),
            nlink: info.nlink(),
        }
    }
}

/// A directory the store owns (keys/, a server's directory, family-mcp/): 0700, ours, real.
pub(crate) fn owned_directory_problem(info: &StoreStat, uid: u32) -> Option<&'static str> {
    match info.kind {
        Kind::Link => Some(LINKED_DIRECTORY),
        Kind::Directory if info.uid != uid => Some(FOREIGN_DIRECTORY),
        Kind::Directory if info.mode & 0o077 != 0 => Some(OPEN_DIRECTORY),
        Kind::Directory => None,
        _ => Some(NOT_A_DIRECTORY),
    }
}

/// A directory above the store must not let another user replace what is below it: owned by this
/// user or root and not writable by group or other. A sticky shared directory such as /tmp passes
/// when the entry below it is this user's or root's, which only they can rename or remove.
pub(crate) fn ancestor_problem(
    info: &StoreStat,
    uid: u32,
    child_uid: Option<u32>,
) -> Option<&'static str> {
    if info.kind != Kind::Directory {
        return Some(NOT_A_DIRECTORY);
    }

    if info.uid != uid && info.uid != 0 {
        return Some(FOREIGN_ANCESTOR);
    }
    let sticky =
        info.mode & STICKY != 0 && child_uid.is_some_and(|child| child == uid || child == 0);

    (info.mode & 0o022 != 0 && !sticky).then_some(WRITABLE_ANCESTOR)
}

/// A key, record or marker file: the same rules and texts as `read_private_bytes`, less the
/// command that the refusal's fix carries. `links` is the number of names it may have, more than
/// one only for a recognised interrupted key publication.
pub(crate) fn private_file_problem(info: &StoreStat, uid: u32, links: u64) -> Option<&'static str> {
    match info.kind {
        Kind::Link => Some(LINK_FILE),
        Kind::File if info.nlink > links => Some(LINKED_FILE),
        Kind::File if info.mode & 0o077 != 0 => Some(OPEN_FILE),
        Kind::File if info.uid != uid => Some(FOREIGN_FILE),
        Kind::File => None,
        _ => Some(NOT_A_FILE),
    }
}

/// The directories the store owns for one of its files: the file's directory, and `family-mcp/`
/// above it when that is the parent (macOS for both, Linux for keys).
pub(crate) fn store_directories(file: &Path) -> Vec<PathBuf> {
    let directory = parent(file);
    let above = parent(directory);

    if above.file_name() == Some(OsStr::new("family-mcp")) {
        vec![above.to_owned(), directory.to_owned()]
    } else {
        vec![directory.to_owned()]
    }
}

/// `lstat`, or `None` when nothing is there.
pub(crate) fn lstat_or_missing(path: &Path) -> Result<Option<Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(info) => Ok(Some(info)),
        Err(error) if matches!(errno(&error), Some(Errno::NOENT | Errno::NOTDIR)) => Ok(None),
        Err(error) => Err(Error::io(
            error,
            "Cannot inspect the secret store. Check its permissions.",
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AclRole {
    Owned,
    Ancestor,
}

/// `from` is the written store directory an ancestor was reached from, for its refusal's path.
struct Checked {
    path: PathBuf,
    info: Metadata,
    role: AclRole,
    from: Option<PathBuf>,
}

/// A file that may carry one recognised extra link, such as a key whose publication crashed.
pub(crate) type AllowLink<'a> = &'a dyn Fn(&Path, &Metadata) -> Result<bool>;

/// What [`check_store_paths`] checks.
#[derive(Default)]
pub(crate) struct StorePaths<'a> {
    pub directories: &'a [PathBuf],
    pub files: &'a [PathBuf],
    /// Directories to create (0700) when missing, after the directories above them passed.
    pub create: bool,
    pub allow_link: Option<AllowLink<'a>>,
}

/// Check the store directories and every directory above them, on both the written path and the
/// path it resolves to, then the files. Creates the directories (0700) that are missing when
/// asked, only after the directories above them passed. Nothing else is created, read or locked.
pub(crate) fn check_store_paths(paths: &StorePaths) -> Result<()> {
    let uid = rustix::process::getuid().as_raw();
    let mut checked = Vec::new();

    for directory in paths.directories {
        checked.extend(ancestors_of(directory, uid)?);
        let mut info = lstat_or_missing(directory)?;

        if info.is_none() && paths.create {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)
                .map_err(|_| {
                    Error::refusal(
                        "Cannot create the store folder. Check the permissions of the folder above it.",
                        directory,
                        None,
                    )
                })?;
            info = lstat_or_missing(directory)?;
        }
        let Some(info) = info else {
            continue;
        };

        if let Some(problem) = owned_directory_problem(&StoreStat::from(&info), uid) {
            return Err(refusal(problem, directory));
        }
        checked.push(Checked {
            path: directory.clone(),
            info,
            role: AclRole::Owned,
            from: None,
        });
    }

    for file in paths.files {
        let Some(mut info) = lstat_or_missing(file)? else {
            continue;
        };
        let mut links = 1;

        if info.is_file()
            && info.nlink() == 2
            && let Some(allow_link) = paths.allow_link
        {
            if allow_link(file, &info)? {
                links = 2;
            } else {
                // Another process may have removed the recognised second name after the lstat
                // above: look once more, and take the file only if it is the same one, now with
                // one name.
                let Some(fresh) = lstat_or_missing(file)? else {
                    continue;
                };

                if fresh.is_file()
                    && (fresh.dev(), fresh.ino()) == (info.dev(), info.ino())
                    && fresh.nlink() == 1
                {
                    info = fresh;
                }
            }
        }

        if let Some(problem) = private_file_problem(&StoreStat::from(&info), uid, links) {
            return Err(refusal(problem, file));
        }
        checked.push(Checked {
            path: file.clone(),
            info,
            role: AclRole::Owned,
            from: None,
        });
    }

    if cfg!(target_os = "macos") {
        check_acls(&checked)?;
    }
    Ok(())
}

/// More expansions than this above one store directory are a loop or an attack; macOS stops at
/// 32.
pub(crate) const MAX_LINKS: usize = 40;

/// The existing directories above `directory`, each checked, along the route the kernel takes:
/// one name at a time from the root, and through every symbolic link on the way, including links
/// inside a link's target and the directories above that target. A link must be this user's or
/// root's. The walk stops at the first missing name; a dangling link or more than `MAX_LINKS`
/// expansions is refused. Paths in the result have no links.
fn ancestors_of(directory: &Path, uid: u32) -> Result<Vec<Checked>> {
    let mut found: Vec<Checked> = Vec::new();
    let absolute = resolve(directory);
    let root = PathBuf::from("/");
    let leaf = absolute
        .file_name()
        .map(OsStr::to_owned)
        .unwrap_or_default();
    let mut names: VecDeque<OsString> = components(parent(&absolute)).into();
    let mut current = root.clone();
    let mut info = lstat(&current)?;
    let mut links = 0;

    loop {
        let next = names.pop_front();
        let name = next.clone().unwrap_or_else(|| leaf.clone());

        if name == "." {
            continue;
        }

        if name == ".." {
            // Its parent was checked on the way down, with this directory as its child.
            current = parent(&current).to_owned();
            info = lstat(&current)?;
            continue;
        }
        let entry = current.join(&name);
        let child = lstat_or_missing(&entry)?;

        if let Some(problem) = ancestor_problem(
            &StoreStat::from(&info),
            uid,
            child.as_ref().map(MetadataExt::uid),
        ) {
            return Err(refusal(problem, shown(&current, &absolute)));
        }
        let checked = Checked {
            path: current.clone(),
            info: info.clone(),
            role: AclRole::Ancestor,
            from: Some(absolute.clone()),
        };

        match found.iter_mut().find(|known| known.path == current) {
            Some(known) => *known = checked,
            None => found.push(checked),
        }

        let (Some(_), Some(child)) = (next, child) else {
            break;
        };

        if child.file_type().is_symlink() {
            if child.uid() != uid && child.uid() != 0 {
                return Err(Error::refusal(
                    FOREIGN_ANCESTOR,
                    shown(&entry, &absolute),
                    None,
                ));
            }
            links += 1;

            if links > MAX_LINKS || fs::metadata(&entry).is_err() {
                return Err(Error::refusal(BROKEN_LINK, shown(&entry, &absolute), None));
            }
            let target = fs::read_link(&entry)
                .map_err(|_| Error::refusal(BROKEN_LINK, shown(&entry, &absolute), None))?;

            // A relative target continues from the link's directory, an absolute one from the
            // root.
            if target.is_absolute() {
                current = root.clone();
                info = lstat(&current)?;
            }

            for name in components(&target).into_iter().rev() {
                names.push_front(name);
            }
            continue;
        }
        current = entry;
        info = child;
    }
    Ok(found)
}

fn lstat(path: &Path) -> Result<Metadata> {
    fs::symlink_metadata(path).map_err(|error| {
        Error::io(
            error,
            "Cannot inspect the secret store. Check its permissions.",
        )
    })
}

/// The names of `path` below its root, `.` and `..` kept for the walk to apply.
fn components(path: &Path) -> Vec<OsString> {
    path.as_os_str()
        .as_bytes()
        .split(|byte| *byte == b'/')
        .filter(|name| !name.is_empty())
        .map(|name| OsStr::from_bytes(name).to_owned())
        .collect()
}

/// The route as the owner wrote it when it names the same entry, else the resolved one.
fn shown(path: &Path, written: &Path) -> PathBuf {
    for prefix in prefixes(written) {
        let same = match prefix.file_name() {
            None => fs::canonicalize(&prefix).is_ok_and(|real| real == path),
            Some(name) => {
                fs::canonicalize(parent(&prefix)).is_ok_and(|real| real.join(name) == path)
            }
        };

        if same {
            return prefix;
        }
    }
    path.to_owned()
}

/// `/a/b` gives `/`, `/a`, `/a/b`.
fn prefixes(path: &Path) -> Vec<PathBuf> {
    let mut prefix = PathBuf::from("/");
    let mut all = vec![prefix.clone()];

    for name in components(path) {
        prefix.push(name);
        all.push(prefix.clone());
    }
    all
}

// Grants that let another user replace or change what is below a directory above the store.
const CHANGING: [&str; 10] = [
    "write",
    "append",
    "add_file",
    "add_subdirectory",
    "delete",
    "delete_child",
    "writeattr",
    "writeextattr",
    "writesecurity",
    "chown",
];

// `ls` answers in milliseconds, but a loaded Mac can stall it: a working one took over 5 s on a
// busy CI runner. The bound only ends a hung child; a listing that runs out still fails closed.
const ACL_TIMEOUT: Duration = Duration::from_secs(20);

/// The ls to run: Apple's, or a test's fake under the test seam.
fn ls() -> PathBuf {
    fake_executable(TEST_LS).unwrap_or_else(|| PathBuf::from("/bin/ls"))
}

type Stamp = (PathBuf, u64, u64, i64, i64, u32);

// An ACL change changes the inode's ctime, so an unchanged inode keeps its last answer.
static ACL_CACHE: Mutex<BTreeSet<Stamp>> = Mutex::new(BTreeSet::new());

fn stamp(entry: &Checked) -> Stamp {
    let info = &entry.info;
    (
        entry.path.clone(),
        info.dev(),
        info.ino(),
        info.ctime(),
        info.ctime_nsec(),
        info.mode(),
    )
}

/// macOS keeps ACLs apart from the mode bits; `ls -le` is the only reader without a native API.
fn check_acls(checked: &[Checked]) -> Result<()> {
    let unknown: Vec<&Checked> = {
        let known = ACL_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        checked
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                !known.contains(&stamp(entry))
                    && checked.iter().position(|first| first.path == entry.path) == Some(*index)
            })
            .map(|(_, entry)| entry)
            .collect()
    };

    let Some(first) = unknown.first() else {
        return Ok(());
    };

    if unknown
        .iter()
        .any(|entry| entry.path.as_os_str().as_bytes().contains(&b'\n'))
    {
        return Err(Error::refusal(
            "The store path contains a line break.",
            &first.path,
            None,
        ));
    }
    let mut args: Vec<&OsStr> = vec![OsStr::new("-ldef"), OsStr::new("--")];
    args.extend(unknown.iter().map(|entry| entry.path.as_os_str()));
    let listing = run_bounded(&ls(), &args, ACL_TIMEOUT, 1_048_576);

    if listing.status != Some(0) {
        return Err(Error::new(Code::Io, ACL_UNREADABLE));
    }
    let entries = acl_entries(&listing.stdout);

    if entries.len() != unknown.len() {
        return Err(Error::new(Code::Io, ACL_UNREADABLE));
    }
    // Only a grant above the store that names a user needs this user's own name.
    let named = unknown.iter().zip(&entries).any(|(entry, lines)| {
        entry.role == AclRole::Ancestor && lines.iter().any(|line| line.contains(": user:"))
    });
    let me = if named { own_name() } else { "" };

    for (entry, lines) in unknown.iter().zip(&entries) {
        if let Some(problem) = acl_problem(lines, entry.role, me) {
            let path = match &entry.from {
                Some(from) => shown(&entry.path, from),
                None => entry.path.clone(),
            };
            return Err(refusal(problem, path));
        }
    }
    ACL_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .extend(unknown.iter().map(|entry| stamp(entry)));
    Ok(())
}

/// This user's name as ACL entries print it; empty when `id` cannot say, which matches no entry,
/// so a grant to an unknown name is refused.
fn own_name() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let run = run_bounded(
            Path::new("/usr/bin/id"),
            &[OsStr::new("-un")],
            Duration::from_secs(5),
            4096,
        );
        match run.status {
            Some(0) => run.stdout.trim_end_matches('\n').to_owned(),
            _ => String::new(),
        }
    })
}

/// The ACL lines of each `ls -lde` entry, in argument order.
pub(crate) fn acl_entries(output: &str) -> Vec<Vec<&str>> {
    let mut entries: Vec<Vec<&str>> = Vec::new();

    for line in output.split('\n') {
        if line.is_empty() {
            continue;
        }

        if line.starts_with(' ') {
            if let Some(entry) = entries.last_mut() {
                entry.push(line);
            }
        } else {
            entries.push(Vec::new());
        }
    }
    entries
}

/// `^ \d+: (\S+)(?: inherited)? (allow|deny) (\S+)$`: the principal, allow, and the permissions.
fn acl_entry(line: &str) -> Option<(&str, bool, &str)> {
    let (number, rest) = line.strip_prefix(' ')?.split_once(": ")?;

    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let words: Vec<&str> = rest.split(' ').collect();
    let (principal, kind, permissions) = match words.as_slice() {
        [principal, kind, permissions] => (*principal, *kind, *permissions),
        [principal, "inherited", kind, permissions] => (*principal, *kind, *permissions),
        _ => return None,
    };
    let plain = |word: &str| !word.is_empty() && !word.chars().any(char::is_whitespace);

    if !plain(principal) || !plain(permissions) {
        return None;
    }
    match kind {
        "allow" => Some((principal, true, permissions)),
        "deny" => Some((principal, false, permissions)),
        _ => None,
    }
}

/// Deny entries pass: a stock home carries `group:everyone deny delete`. A store directory or file
/// refuses every allow entry; a directory above it refuses one that lets another user change it.
/// An entry this check cannot read is refused rather than guessed at.
pub(crate) fn acl_problem(lines: &[&str], role: AclRole, me: &str) -> Option<&'static str> {
    let message = match role {
        AclRole::Owned => OWNED_ACL,
        AclRole::Ancestor => ANCESTOR_ACL,
    };

    for line in lines {
        let Some((principal, allow, permissions)) = acl_entry(line) else {
            return Some(message);
        };

        if !allow {
            continue;
        }

        if role == AclRole::Owned {
            return Some(message);
        }

        if !me.is_empty() && principal.strip_prefix("user:") == Some(me) {
            continue;
        }

        if permissions
            .split(',')
            .any(|permission| CHANGING.contains(&permission))
        {
            return Some(message);
        }
    }
    None
}

/// The store's recognised temporaries of `path`: `<name>.<uuid>.tmp` beside it.
pub(crate) fn temporaries_of(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(name) = path.file_name() else {
        return Ok(Vec::new());
    };
    let mut prefix = name.as_bytes().to_vec();
    prefix.push(b'.');
    let directory = parent(path);
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if errno(&error) == Some(Errno::NOENT) => return Ok(Vec::new()),
        Err(error) => {
            return Err(Error::io(
                error,
                "Cannot list the directory. Check its permissions.",
            ));
        }
    };
    let mut found = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|error| {
            Error::io(error, "Cannot list the directory. Check its permissions.")
        })?;
        let name = entry.file_name();
        let own = name
            .as_bytes()
            .strip_prefix(prefix.as_slice())
            .and_then(|rest| rest.strip_suffix(b".tmp"))
            .and_then(|uuid| std::str::from_utf8(uuid).ok())
            .is_some_and(crate::files::is_uuid);

        if own {
            found.push(directory.join(name));
        }
    }
    found.sort();
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 501;

    const OTHER: u32 = UID + 4242;

    fn fake(kind: Kind, mode: u32, uid: u32) -> StoreStat {
        StoreStat {
            kind,
            mode,
            uid,
            nlink: 1,
        }
    }

    #[test]
    fn store_directory_and_ancestor_decisions_are_pure() {
        let directory = |mode, uid| fake(Kind::Directory, mode, uid);
        assert_eq!(owned_directory_problem(&directory(0o40700, UID), UID), None);
        assert_eq!(
            owned_directory_problem(&fake(Kind::Link, 0o120777, UID), UID),
            Some(LINKED_DIRECTORY)
        );
        assert_eq!(
            owned_directory_problem(&directory(0o40700, OTHER), UID),
            Some(FOREIGN_DIRECTORY)
        );
        // A root-owned store directory is refused like any other owner.
        assert_eq!(
            owned_directory_problem(&directory(0o40700, 0), UID),
            Some(FOREIGN_DIRECTORY)
        );
        assert_eq!(
            owned_directory_problem(&directory(0o40750, UID), UID),
            Some(OPEN_DIRECTORY)
        );
        assert_eq!(
            owned_directory_problem(&directory(0o40701, UID), UID),
            Some(OPEN_DIRECTORY)
        );
        assert_eq!(
            owned_directory_problem(&fake(Kind::File, 0o100600, UID), UID),
            Some(NOT_A_DIRECTORY)
        );

        assert_eq!(
            ancestor_problem(&directory(0o40755, UID), UID, Some(UID)),
            None
        );
        assert_eq!(
            ancestor_problem(&directory(0o40755, 0), UID, Some(UID)),
            None
        );
        assert_eq!(
            ancestor_problem(&directory(0o40750, UID), UID, Some(UID)),
            None
        );
        for mode in [0o40775, 0o40757] {
            assert_eq!(
                ancestor_problem(&directory(mode, UID), UID, Some(UID)),
                Some(WRITABLE_ANCESTOR)
            );
        }
        assert_eq!(
            ancestor_problem(&directory(0o40775, 0), UID, Some(UID)),
            Some(WRITABLE_ANCESTOR)
        );
        assert_eq!(
            ancestor_problem(&directory(0o40755, OTHER), UID, Some(UID)),
            Some(FOREIGN_ANCESTOR)
        );
        // A sticky shared directory such as /tmp protects an entry only its owner or root can
        // move.
        let sticky = directory(0o41777, 0);
        assert_eq!(ancestor_problem(&sticky, UID, Some(UID)), None);
        assert_eq!(ancestor_problem(&sticky, UID, Some(0)), None);
        assert_eq!(
            ancestor_problem(&sticky, UID, Some(OTHER)),
            Some(WRITABLE_ANCESTOR)
        );
        assert_eq!(
            ancestor_problem(&sticky, UID, None),
            Some(WRITABLE_ANCESTOR)
        );
    }

    #[test]
    fn private_files_follow_the_reader_rules() {
        let file = |mode, uid, nlink| StoreStat {
            kind: Kind::File,
            mode,
            uid,
            nlink,
        };
        assert_eq!(private_file_problem(&file(0o100600, UID, 1), UID, 1), None);
        assert_eq!(private_file_problem(&file(0o100600, UID, 2), UID, 2), None);
        assert_eq!(
            private_file_problem(&file(0o100600, UID, 2), UID, 1),
            Some(LINKED_FILE)
        );
        assert_eq!(
            private_file_problem(&file(0o100640, UID, 1), UID, 1),
            Some(OPEN_FILE)
        );
        assert_eq!(
            private_file_problem(&file(0o100600, OTHER, 1), UID, 1),
            Some(FOREIGN_FILE)
        );
        assert_eq!(
            private_file_problem(&fake(Kind::Link, 0o120777, UID), UID, 1),
            Some(LINK_FILE)
        );
        assert_eq!(
            private_file_problem(&fake(Kind::Directory, 0o40700, UID), UID, 1),
            Some(NOT_A_FILE)
        );
        // Each refusal with a command names it; the others have none.
        assert_eq!(fix(OPEN_FILE), Some("chmod 600"));
        assert_eq!(fix(OPEN_DIRECTORY), Some("chmod 700"));
        assert_eq!(fix(WRITABLE_ANCESTOR), Some("chmod go-w"));
        assert_eq!(fix(OWNED_ACL), Some("chmod -N"));
        assert_eq!(fix(LINKED_FILE), None);
        assert_eq!(fix(ANCESTOR_ACL), None);
    }

    #[test]
    fn acl_entries_deny_passes_any_grant_on_the_store_and_a_changing_grant_above_it_fail() {
        let listing = [
            "drwxr-x---+ 152 me  staff  4864 Oct  9 14:47 /Users/me",
            " 0: group:everyone deny delete",
            "drwx------  3 me  staff  96 Oct  9 14:47 /Users/me/Library/Application Support/family-mcp",
            "-rw-------+ 1 me  staff  32 Oct  9 14:47 /Users/me/key",
            " 0: user:other allow read",
            " 1: group:everyone inherited deny delete",
            "",
        ]
        .join("\n");
        let entries = acl_entries(&listing);
        assert_eq!(entries.len(), 3);
        let (home, root, key) = (&entries[0], &entries[1], &entries[2]);
        assert_eq!(home, &[" 0: group:everyone deny delete"]);
        assert!(root.is_empty());
        assert_eq!(key.len(), 2);

        for role in [AclRole::Owned, AclRole::Ancestor] {
            let message = match role {
                AclRole::Owned => OWNED_ACL,
                AclRole::Ancestor => ANCESTOR_ACL,
            };
            assert_eq!(acl_problem(home, role, "me"), None);
            assert_eq!(acl_problem(&[], role, "me"), None);
            assert_eq!(
                acl_problem(&[" 0: something unexpected"], role, "me"),
                Some(message)
            );
        }
        assert_eq!(acl_problem(key, AclRole::Owned, "me"), Some(OWNED_ACL));
        assert_eq!(
            acl_problem(&[" 0: user:me allow read"], AclRole::Owned, "me"),
            Some(OWNED_ACL)
        );
        assert_eq!(acl_problem(key, AclRole::Ancestor, "me"), None);
        assert_eq!(
            acl_problem(
                &[" 0: user:me allow add_file,delete_child"],
                AclRole::Ancestor,
                "me"
            ),
            None
        );
        // Without a known name, a user's grant is judged like anyone else's.
        assert_eq!(
            acl_problem(&[" 0: user:me allow add_file"], AclRole::Ancestor, ""),
            Some(ANCESTOR_ACL)
        );

        for permission in [
            "add_file",
            "delete_child",
            "add_subdirectory",
            "writesecurity",
        ] {
            let line = format!(" 0: group:staff inherited allow list,{permission}");
            assert_eq!(
                acl_problem(&[line.as_str()], AclRole::Ancestor, "me"),
                Some(ANCESTOR_ACL)
            );
        }
        for unreadable in [
            " 0: user:me  allow read",
            "  0: user:me allow read",
            " x: user:me allow read",
            " 0: user:me sometimes read",
            " 0: user:me inherited allow",
        ] {
            assert_eq!(
                acl_problem(&[unreadable], AclRole::Ancestor, "me"),
                Some(ANCESTOR_ACL),
                "{unreadable}"
            );
        }
    }

    #[test]
    fn store_directories_include_family_mcp_only_as_the_parent() {
        assert_eq!(
            store_directories(Path::new("/a/family-mcp/keys/x.key")),
            [
                PathBuf::from("/a/family-mcp"),
                PathBuf::from("/a/family-mcp/keys")
            ]
        );
        assert_eq!(
            store_directories(Path::new("/a/.config/abler-mcp/session.enc")),
            [PathBuf::from("/a/.config/abler-mcp")]
        );
    }

    #[test]
    fn a_key_whose_recognised_second_name_another_process_just_removed_passes() {
        use std::io::Write;

        use crate::files::{create_private, uuid};
        use crate::keys::key_temporary;

        let root =
            std::env::temp_dir().join(format!("family-store-allow-link-{}", uuid().unwrap()));
        DirBuilder::new().mode(0o700).create(&root).unwrap();
        let key = root.join("test-mcp.default.key");
        create_private(&key).unwrap().write_all(&[7; 32]).unwrap();
        let temporary = root.join(format!("test-mcp.default.key.{}.tmp", uuid().unwrap()));
        fs::hard_link(&key, &temporary).unwrap();

        // The other process's recovery lands between this check's lstat and its own lookup.
        let recovered = |file: &Path, info: &Metadata| -> Result<bool> {
            let _ = fs::remove_file(&temporary);
            Ok(key_temporary(file, info)?.is_some())
        };
        let files = [key.clone()];
        let check = || {
            check_store_paths(&StorePaths {
                directories: &[],
                files: &files,
                create: false,
                allow_link: Some(&recovered),
            })
        };
        check().unwrap();
        assert_eq!(fs::symlink_metadata(&key).unwrap().nlink(), 1);

        // A second name that stays is still refused, also when the hook does not recognise it.
        let stray = root.join("stray.key");
        fs::hard_link(&key, &stray).unwrap();
        let refused = check().unwrap_err();
        assert_eq!(refused.code, Code::UnsafeFile);
        assert_eq!(refused.message, LINKED_FILE);
        assert_eq!(refused.path(), Some(key.as_path()));

        for file in [&stray, &key] {
            fs::remove_file(file).unwrap();
        }
        fs::remove_dir(&root).unwrap();
    }
}
