use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{FileMode, rename_with_retry, write_atomic, write_atomic_with};

fn scratch(name: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("nyan-fs-{name}-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_new_file_and_a_replaced_file_hold_exactly_the_new_bytes_and_leave_no_temporary() {
    let dir = scratch("basic");
    let target = dir.join("file.txt");
    write_atomic(&target, b"first", FileMode::Preserve).expect("create");
    assert_eq!(fs::read(&target).expect("read"), b"first");
    write_atomic(&target, b"second\r\n\xe4\xb8\x96", FileMode::Preserve).expect("replace");
    assert_eq!(fs::read(&target).expect("read"), b"second\r\n\xe4\xb8\x96");
    assert_eq!(names(&dir), ["file.txt"]);
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn a_reader_never_sees_a_torn_file_while_writers_replace_it() {
    let dir = scratch("torn");
    let target = dir.join("shared.txt");
    let variants: Vec<Vec<u8>> = (0..4)
        .map(|index| format!("writer-{index}\n").repeat(20_000).into_bytes())
        .collect();
    write_atomic(&target, &variants[0], FileMode::Preserve).expect("seed");

    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reader = {
        let (target, variants, stop) = (target.clone(), variants.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0_u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let seen = fs::read(&target).expect("the file always exists");
                assert!(
                    variants.contains(&seen),
                    "a reader saw {} bytes that no writer wrote",
                    seen.len()
                );
                reads += 1;
            }
            reads
        })
    };
    let writers: Vec<_> = variants
        .iter()
        .map(|bytes| {
            let (target, bytes) = (target.clone(), bytes.clone());
            std::thread::spawn(move || {
                for _ in 0..40 {
                    write_atomic(&target, &bytes, FileMode::Preserve).expect("write");
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().expect("writer");
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().expect("reader") > 0);
    assert_eq!(names(&dir), ["shared.txt"], "no temporary file remains");
    fs::remove_dir_all(dir).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn modes_are_kept_or_forced_as_asked() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("mode");
    let kept = dir.join("kept");
    fs::write(&kept, "x").expect("seed");
    fs::set_permissions(&kept, fs::Permissions::from_mode(0o640)).expect("chmod");
    write_atomic(&kept, b"y", FileMode::Preserve).expect("replace");
    assert_eq!(
        fs::metadata(&kept).expect("metadata").permissions().mode() & 0o777,
        0o640
    );

    let secret = dir.join("secret");
    write_atomic(&secret, b"s", FileMode::Exactly(0o600)).expect("create");
    assert_eq!(
        fs::metadata(&secret)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    fs::set_permissions(&secret, fs::Permissions::from_mode(0o644)).expect("chmod");
    write_atomic(&secret, b"t", FileMode::Exactly(0o600)).expect("replace");
    assert_eq!(
        fs::metadata(&secret)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "an exact mode is applied to a replaced file too"
    );
    fs::remove_dir_all(dir).expect("cleanup");
}

#[cfg(unix)]
#[test]
fn a_symlink_stays_a_symlink_and_its_target_is_replaced() {
    let dir = scratch("link");
    let real = dir.join("real.txt");
    let link = dir.join("link.txt");
    fs::write(&real, "old").expect("seed");
    std::os::unix::fs::symlink(&real, &link).expect("symlink");
    write_atomic(&link, b"new", FileMode::Preserve).expect("write through the link");
    assert!(fs::symlink_metadata(&link).expect("link").is_symlink());
    assert_eq!(fs::read(&real).expect("target"), b"new");
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn a_failed_rename_leaves_the_old_file_and_removes_the_temporary() {
    let dir = scratch("failed");
    let target = dir.join("file.txt");
    fs::write(&target, "old").expect("seed");
    let error = write_atomic_with(&target, b"new", FileMode::Preserve, &mut |_, _| {
        Err(io::Error::other("disk on fire"))
    })
    .expect_err("rename fails");
    assert_eq!(error.to_string(), "disk on fire");
    assert_eq!(fs::read(&target).expect("read"), b"old");
    assert_eq!(names(&dir), ["file.txt"]);
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn a_directory_that_refuses_new_names_falls_back_to_writing_the_file_in_place() {
    let dir = scratch("denied");
    let target = dir.join("file.txt");
    fs::write(&target, "old").expect("seed");
    write_atomic_with(&target, b"new", FileMode::Preserve, &mut |_, _| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    })
    .expect("in-place fallback");
    assert_eq!(fs::read(&target).expect("read"), b"new");
    assert_eq!(names(&dir), ["file.txt"]);

    let missing = dir.join("absent.txt");
    let error = write_atomic_with(&missing, b"new", FileMode::Preserve, &mut |_, _| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    })
    .expect_err("no file to fall back on");
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!missing.exists());
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn only_transient_errors_are_retried_and_only_a_limited_number_of_times() {
    let transient = |error: &io::Error| error.kind() == io::ErrorKind::WouldBlock;
    let from = Path::new("a");
    let to = Path::new("b");

    let calls = Cell::new(0);
    rename_with_retry(
        from,
        to,
        &mut |_, _| {
            calls.set(calls.get() + 1);
            if calls.get() < 4 {
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            } else {
                Ok(())
            }
        },
        &transient,
        false,
    )
    .expect("succeeds on the fourth attempt");
    assert_eq!(calls.get(), 4);

    let calls = Cell::new(0);
    let error = rename_with_retry(
        from,
        to,
        &mut |_, _| {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::NotFound))
        },
        &transient,
        false,
    )
    .expect_err("not transient");
    assert_eq!((calls.get(), error.kind()), (1, io::ErrorKind::NotFound));

    let calls = Cell::new(0);
    rename_with_retry(
        from,
        to,
        &mut |_, _| {
            calls.set(calls.get() + 1);
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        },
        &transient,
        false,
    )
    .expect_err("never clears");
    assert_eq!(calls.get(), 11, "one attempt plus ten retries");
}

#[cfg(windows)]
#[test]
fn windows_access_denied_sharing_and_lock_violations_are_transient() {
    for code in [5, 32, 33] {
        assert!(super::is_transient_rename_error(
            &io::Error::from_raw_os_error(code)
        ));
    }
    assert!(!super::is_transient_rename_error(
        &io::Error::from_raw_os_error(2)
    ));
}

#[cfg(not(windows))]
#[test]
fn nothing_is_transient_off_windows() {
    assert!(!super::is_transient_rename_error(
        &io::Error::from_raw_os_error(5)
    ));
}
