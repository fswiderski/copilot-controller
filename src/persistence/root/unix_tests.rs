use super::super::tests::{Fixture, exercise_process_lock};
use super::*;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};

fn stat_with(
    mode: libc::mode_t,
    uid: libc::uid_t,
    dev: libc::dev_t,
    links: libc::nlink_t,
) -> libc::stat {
    // SAFETY: libc::stat is a plain C data structure; all-zero fields are valid.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    stat.st_mode = mode;
    stat.st_uid = uid;
    stat.st_dev = dev;
    stat.st_nlink = links;
    stat
}

#[test]
fn synthetic_ancestor_owner_permissions_and_file_types() {
    let uid = 123;
    let dir = libc::S_IFDIR | 0o700;
    assert!(check_private_directory(&stat_with(dir, uid, 7, 1), uid).is_ok());
    for mode in [
        libc::S_IFDIR | 0o750,
        libc::S_IFDIR | 0o2700,
        libc::S_IFREG | 0o600,
    ] {
        assert_eq!(
            check_private_directory(&stat_with(mode, uid, 7, 1), uid)
                .unwrap_err()
                .kind(),
            ErrorKind::UnsafeRoot
        );
    }
    assert!(check_private_directory(&stat_with(dir, uid + 1, 7, 1), uid).is_err());
    assert!(check_ancestor(&stat_with(libc::S_IFDIR | 0o755, uid, 7, 1), uid).is_ok());
    assert!(check_ancestor(&stat_with(libc::S_IFDIR | 0o1777, 0, 7, 1), uid).is_ok());
    for (mode, owner) in [
        (libc::S_IFDIR | 0o775, uid),
        (libc::S_IFDIR | 0o1777, uid),
        (libc::S_IFDIR | 0o777, 0),
        (libc::S_IFDIR | 0o755, uid + 1),
        (libc::S_IFREG | 0o755, uid),
    ] {
        assert!(check_ancestor(&stat_with(mode, owner, 7, 1), uid).is_err());
    }
    let regular = libc::S_IFREG | 0o600;
    assert!(check_private_file(&stat_with(regular, uid, 7, 1), uid, 7, false).is_ok());
    assert!(check_private_file(&stat_with(regular, uid, 7, 0), uid, 7, true).is_ok());
    assert!(check_tail_file(&stat_with(libc::S_IFREG | 0o644, uid + 1, 8, 1)).is_ok());
    assert!(check_tail_file(&stat_with(libc::S_IFREG | 0o644, uid, 7, 2)).is_err());
    assert!(check_tail_file(&stat_with(libc::S_IFLNK | 0o777, uid, 7, 1)).is_err());
    for bad in [
        stat_with(libc::S_IFREG | 0o640, uid, 7, 1),
        stat_with(libc::S_IFREG | 0o4600, uid, 7, 1),
        stat_with(libc::S_IFLNK | 0o600, uid, 7, 1),
        stat_with(libc::S_IFIFO | 0o600, uid, 7, 1),
        stat_with(regular, uid, 7, 2),
        stat_with(regular, uid, 7, 0),
        stat_with(regular, uid + 1, 7, 1),
        stat_with(regular, uid, 8, 1),
    ] {
        assert_eq!(
            check_private_file(&bad, uid, 7, false).unwrap_err().kind(),
            ErrorKind::UnsafeFile
        );
    }
}

#[cfg(target_os = "macos")]
#[test]
fn synthetic_apfs_filesystem_flags_are_fail_closed() {
    // SAFETY: statfs is a plain C data structure; all-zero fields are valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    for (target, byte) in stat.f_fstypename.iter_mut().zip(b"apfs\0") {
        *target = *byte as libc::c_char;
    }
    stat.f_flags = libc::MNT_LOCAL as u32;
    assert!(filesystem_policy(&stat).is_ok());
    stat.f_flags |= libc::MNT_IGNORE_OWNERSHIP as u32;
    assert_eq!(
        filesystem_policy(&stat).unwrap_err().operation(),
        "filesystem_ownership_disabled"
    );
    stat.f_flags = 0;
    assert_eq!(
        filesystem_policy(&stat).unwrap_err().kind(),
        ErrorKind::UnsupportedFilesystem
    );
    stat.f_flags = libc::MNT_LOCAL as u32;
    stat.f_fstypename[0] = b'n' as libc::c_char;
    assert!(filesystem_policy(&stat).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn synthetic_linux_filesystem_allowlist_is_fail_closed() {
    // SAFETY: statfs is a plain C data structure; all-zero fields are valid.
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    for magic in [0xef53_u64, 0x5846_5342, 0x9123_683e] {
        stat.f_type = magic as _;
        assert!(filesystem_policy(&stat).is_ok());
    }
    for magic in [0x794c_7630, 0x6969, 0x6573_5546, 0] {
        stat.f_type = magic as _;
        assert_eq!(
            filesystem_policy(&stat).unwrap_err().kind(),
            ErrorKind::UnsupportedFilesystem
        );
    }
}

#[test]
fn synthetic_descriptor_checks_use_only_the_unique_fixture() {
    let fixture = Fixture::new();
    let path = fixture.root.join("file");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let file = options.open(&path).unwrap();
    let stat = stat_fd(&file, "synthetic_stat").unwrap();
    let dev = device(&stat);
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    assert!(check_private_file(&stat, uid, dev, false).is_ok());
    sync_file(&file).unwrap();
    check_acl(&file).unwrap();

    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    assert!(
        check_private_file(&stat_fd(&file, "synthetic_stat").unwrap(), uid, dev, false).is_err()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&path, fixture.root.join("hard")).unwrap();
    assert!(
        check_private_file(&stat_fd(&file, "synthetic_stat").unwrap(), uid, dev, false).is_err()
    );
    symlink("file", fixture.root.join("link")).unwrap();
    let symlink_stat = fs::symlink_metadata(fixture.root.join("link")).unwrap();
    assert!(symlink_stat.file_type().is_symlink());

    let directory = StdFile::open(&fixture.root).unwrap();
    directory.sync_all().unwrap();
    let linked = stat_at(&directory, c"link").unwrap().unwrap();
    assert!(check_tail_file(&linked).is_err());
    assert!(check_private_file(&linked, uid, dev, false).is_err());
    let linked = stat_at(&directory, c"hard").unwrap().unwrap();
    assert!(check_tail_file(&linked).is_err());
    assert!(check_private_file(&linked, uid, dev, false).is_err());
    let old = identity(&stat_fd(&directory, "synthetic_root").unwrap());
    fs::rename(&fixture.root, fixture.path.join("moved-root")).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&fixture.root)
        .unwrap();
    let new_directory = StdFile::open(&fixture.root).unwrap();
    assert_ne!(
        old,
        identity(&stat_fd(&new_directory, "synthetic_root").unwrap())
    );
}

fn descriptor_tail_snapshot(file: &StdFile) -> super::super::TailMetadata {
    let stat = stat_fd(file, "synthetic_tail_stat").unwrap();
    let (len, change) = tail_snapshot(&stat).unwrap();
    super::super::TailMetadata {
        identity: super::super::TailIdentity(identity(&stat)),
        len,
        change: super::super::TailChangeIdentity(change),
    }
}

#[test]
fn synthetic_tail_read_rejects_mixed_same_size_bytes_without_committing_snapshot() {
    let fixture = Fixture::new();
    let path = fixture.root.join("events");
    fs::write(&path, b"old\nold\n").unwrap();
    let mut file = StdFile::open(&path).unwrap();
    let mut observed = descriptor_tail_snapshot(&file);
    let original = observed;
    let error = super::super::read_tail_at(&mut observed, original, 0, || {
        let mut bytes = vec![0; 8];
        file.read_exact(&mut bytes[..4]).unwrap();
        fs::write(&path, b"new\nnew\n").unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(123))
            .unwrap();
        file.read_exact(&mut bytes[4..]).unwrap();
        assert_eq!(bytes, b"old\nnew\n");
        Ok((bytes, descriptor_tail_snapshot(&file)))
    })
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Changed);
    assert_eq!(error.operation(), "tail_modified");
    assert_eq!(observed, original);
}

#[test]
fn synthetic_tail_eof_rewrite_with_restored_mtime_is_not_an_empty_poll() {
    let fixture = Fixture::new();
    let path = fixture.root.join("events");
    fs::write(&path, b"old\n").unwrap();
    let original_time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(789);
    let writer = OpenOptions::new().write(true).open(&path).unwrap();
    writer.set_modified(original_time).unwrap();
    let mut file = StdFile::open(&path).unwrap();
    let mut observed = descriptor_tail_snapshot(&file);
    let before = observed;
    assert_eq!(
        super::super::read_tail_at(&mut observed, before, 0, || {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            Ok((bytes, descriptor_tail_snapshot(&file)))
        })
        .unwrap(),
        b"old\n"
    );
    std::thread::sleep(std::time::Duration::from_millis(1100));
    fs::write(&path, b"new\n").unwrap();
    writer.set_modified(original_time).unwrap();
    let after = descriptor_tail_snapshot(&file);
    assert_eq!(file.metadata().unwrap().modified().unwrap(), original_time);
    assert_eq!(before.identity, after.identity);
    assert_eq!(before.len, after.len);
    assert_ne!(before.change, after.change);
    let error = super::super::read_tail_at(&mut observed, after, 4, || {
        panic!("same-size rewrite must be rejected before attempting an EOF read")
    })
    .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Changed);
    assert_eq!(observed, before);

    // A caller resets its reducer and snapshot after source change. Normal
    // appends must still be readable rather than treated as same-size rewrites.
    observed = after;
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"appended\n")
        .unwrap();
    let appended = descriptor_tail_snapshot(&file);
    assert_eq!(
        super::super::read_tail_at(&mut observed, appended, 4, || {
            file.seek(SeekFrom::Start(4)).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            Ok((bytes, descriptor_tail_snapshot(&file)))
        })
        .unwrap(),
        b"appended\n"
    );
    assert_eq!(observed, appended);
}

#[test]
fn synthetic_native_rename_is_exclusive_and_keeps_old_reader() {
    let fixture = Fixture::new();
    let directory = StdFile::open(&fixture.root).unwrap();
    let create = |name: &str, bytes: &[u8]| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true).mode(0o600);
        let mut file = options.open(fixture.root.join(name)).unwrap();
        file.write_all(bytes).unwrap();
        sync_file(&file).unwrap();
    };
    create("initial", b"first");
    let dir = directory.as_raw_fd();
    assert_eq!(rename_exclusive(dir, c"initial", c"target"), 0);
    directory.sync_all().unwrap();
    let mut old = StdFile::open(fixture.root.join("target")).unwrap();
    create("candidate", b"second");
    assert_eq!(rename_exclusive(dir, c"candidate", c"target"), -1);
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::EEXIST)
    );
    assert_eq!(fs::read(fixture.root.join("candidate")).unwrap(), b"second");
    // SAFETY: both names are owned test files relative to the fixture directory.
    assert_eq!(
        unsafe { libc::renameat(dir, c"candidate".as_ptr(), dir, c"target".as_ptr()) },
        0
    );
    directory.sync_all().unwrap();
    let mut old_bytes = Vec::new();
    old.read_to_end(&mut old_bytes).unwrap();
    assert_eq!(old_bytes, b"first");
    assert_eq!(fs::read(fixture.root.join("target")).unwrap(), b"second");
}

#[test]
fn synthetic_relative_file_and_directory_operations_reject_unsafe_entries() {
    let fixture = Fixture::new();
    let directory = StdFile::open(&fixture.root).unwrap();
    let root = Root {
        identity: identity(&stat_fd(&directory, "synthetic_root").unwrap()),
        directory,
        components: Vec::new(),
        // SAFETY: geteuid has no preconditions.
        uid: unsafe { libc::geteuid() },
    };
    let lock = Name::new("lock").unwrap();
    let mut created = root.create_file(&lock).unwrap();
    root.check_file(Some(&lock), &created, false).unwrap();
    assert!(root.is_empty_except(&lock).unwrap());
    assert!(root.is_empty_except(&lock).unwrap());
    assert_eq!(
        root.create_file(&lock).err().unwrap().kind(),
        ErrorKind::AlreadyExists
    );
    created.file.write_all(b"synthetic").unwrap();
    sync_file(&created.file).unwrap();
    root.sync_dir().unwrap();
    fs::write(fixture.root.join("extra"), b"x").unwrap();
    assert!(!root.is_empty_except(&lock).unwrap());
    fs::remove_file(fixture.root.join("extra")).unwrap();
    assert!(root.is_empty_except(&lock).unwrap());

    symlink("lock", fixture.root.join("symlink")).unwrap();
    fs::hard_link(fixture.root.join("lock"), fixture.root.join("hard")).unwrap();
    let fifo = CString::new(fixture.root.join("fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string identifies a file inside this unique fixture.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    for bad in ["symlink", "lock", "hard", "fifo"] {
        let error = match root.open_file(&Name::new(bad).unwrap(), false) {
            Err(error) => error,
            Ok(_) => panic!("unsafe synthetic entry was accepted"),
        };
        assert_eq!(error.kind(), ErrorKind::UnsafeFile, "{bad}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn synthetic_native_acl_is_rejected() {
    let fixture = Fixture::new();
    let path = fixture.root.join("acl");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let file = options.open(&path).unwrap();
    assert!(check_acl(&file).is_ok());
    let status = std::process::Command::new("/bin/chmod")
        .args(["+a", "everyone allow read"])
        .arg(&path)
        .status()
        .expect("invoke system chmod on the unique fixture");
    assert!(
        status.success(),
        "fixture filesystem does not support ACL setup"
    );
    assert_eq!(check_acl(&file).unwrap_err().kind(), ErrorKind::UnsafeAcl);
}

#[test]
fn synthetic_os_lock_contention_is_nonblocking() {
    let fixture = Fixture::new();
    let path = fixture.root.join("lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let first = options.open(&path).unwrap();
    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    try_lock(&first).unwrap();
    assert_eq!(try_lock(&second).unwrap_err().kind(), ErrorKind::Busy);
    drop(first);
    try_lock(&second).unwrap();
}

#[test]
fn synthetic_lock_child() {
    let Some(path) = std::env::var_os("COPILOT_PRIVATE_ROOT_SYNTHETIC_LOCK_CHILD") else {
        return;
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert_eq!(try_lock(&file).unwrap_err().kind(), ErrorKind::Busy);
    println!("LOCK_CHILD_BUSY");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
    try_lock(&file).unwrap();
    println!("LOCK_CHILD_ACQUIRED");
    io::stdout().flush().unwrap();
    line.clear();
    io::stdin().read_line(&mut line).unwrap();
}

#[test]
fn synthetic_lock_has_bounded_cross_process_contention() {
    let fixture = Fixture::new();
    let path = fixture.root.join("process-lock");
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true).mode(0o600);
    let first = options.open(&path).unwrap();
    let second = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    try_lock(&first).unwrap();
    exercise_process_lock(
        "unix::tests::synthetic_lock_child",
        "COPILOT_PRIVATE_ROOT_SYNTHETIC_LOCK_CHILD",
        &path,
        || drop(first),
        || assert_eq!(try_lock(&second).unwrap_err().kind(), ErrorKind::Busy),
    );
    try_lock(&second).unwrap();
}
