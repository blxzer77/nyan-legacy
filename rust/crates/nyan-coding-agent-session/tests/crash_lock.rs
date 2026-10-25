//! A session lock belongs to the operating system, not to a file's existence: when the
//! process holding it is killed, the session opens again at once.
//!
//! This is its own test binary on purpose. A flock is shared with every copy of the file
//! descriptor, and `fork` copies descriptors for a moment before `exec`; other tests
//! locking sessions on other threads of the same process would make this one flaky.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nyan_coding_agent_core::{AgentMode, SessionId};
use nyan_coding_agent_session::{
    FileSessionStore, SessionDescriptor, SessionOpenRequest, SessionStore, SessionStoreError,
};

fn request(root: &Path) -> SessionOpenRequest {
    SessionOpenRequest {
        descriptor: SessionDescriptor::persistent(
            SessionId::new("held").expect("session id"),
            "/tmp/project",
        ),
        mode: AgentMode::Print,
        storage_root: Some(root.to_owned()),
    }
}

fn scratch_root() -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("nyan-crash-lock-{}-{nonce}", std::process::id()))
}

/// Runs only as the child of `a_session_lock_held_by_a_killed_process_is_released`: it
/// takes the session and then waits to be killed.
#[test]
fn lock_holder_child() {
    let Ok(root) = std::env::var("NYAN_LOCK_HOLDER_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let store = FileSessionStore::new(&root);
    let _opened = store.open(request(&root)).expect("child takes the session");
    println!("LOCKED");
    std::thread::sleep(std::time::Duration::from_secs(120));
}

#[test]
fn a_session_lock_held_by_a_killed_process_is_released() {
    let root = scratch_root();
    std::fs::create_dir_all(&root).expect("root");
    let mut child = Command::new(std::env::current_exe().expect("test binary"))
        .args(["--exact", "lock_holder_child", "--nocapture"])
        .env("NYAN_LOCK_HOLDER_ROOT", &root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the holder");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();
    assert!(
        lines.any(|line| line.is_ok_and(|line| line == "LOCKED")),
        "the child never reported the lock"
    );

    let store = FileSessionStore::new(&root);
    assert!(
        matches!(
            store.open(request(&root)),
            Err(SessionStoreError::Locked { .. })
        ),
        "a live holder keeps the session locked"
    );

    child.kill().expect("kill the holder");
    child.wait().expect("reap the holder");
    let reopened = store
        .open(request(&root))
        .expect("the kernel released the killed process's lock");
    drop(reopened);
    std::fs::remove_dir_all(root).expect("remove the scratch root");
}
