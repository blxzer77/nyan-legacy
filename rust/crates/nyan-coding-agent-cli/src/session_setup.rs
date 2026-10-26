//! Chooses and opens the session for a run, from `--no-session`, `--continue`,
//! `--session`, `--session-id`, `--session-dir`, and `--name`.
//!
//! Behavior follows `docs/sessions.md`: sessions are saved unless `--no-session`;
//! `--continue` reopens the working directory's newest session or starts a new
//! one; `--session-id` opens that exact id and creates it when it is missing;
//! `--session` takes an exact id or a unique prefix; `--session-dir` moves the
//! store. Each working directory has its own store, named by a hash of the path.
//!
//! Sessions live in `sessions` under the agent directory, in the Rust journal
//! format: one store, not one per generation of the product. The older stores,
//! legacy JSONL (the same tree, `*.jsonl` files) and `sessions-durable`, are
//! import sources only; this code never reads or writes them. The TypeScript CLI
//! lists only `*.jsonl` files, so the `.session` files here do not disturb it.
//! The format version is in each file's header, not in the directory name.
//!
//! `--resume` (a picker) is not implemented and is refused; `--fork` opens a
//! new Rust-native session at a user-message entry.

use std::path::{Path, PathBuf};

use nyan_coding_agent_core::{AgentMode, SessionId};
use nyan_coding_agent_session::{
    FileSessionStore, NativeSession, SessionDescriptor, SessionEntryId, SessionOpenRequest,
    SessionStoreError,
};

use crate::args::{Args, normalize_session_name};
use crate::headless::new_session_id;

/// The directory under the agent directory that holds Rust-native sessions.
pub const SESSION_STORE_DIR: &str = "sessions";

/// Returns the stable identity used to partition sessions by working directory.
/// Existing paths are canonicalized so symlinked spellings share one session
/// directory. Windows paths are case-insensitive and may be returned with the
/// `\\?\\` device prefix; neither detail belongs in the persisted key.
fn cwd_identity(cwd: &Path) -> String {
    let path = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_owned());
    let mut identity = path.to_string_lossy().replace('\\', "/");
    if let Some(rest) = identity.strip_prefix("//?/UNC/") {
        identity = format!("//{rest}");
    } else if let Some(rest) = identity.strip_prefix("//?/") {
        identity = rest.to_owned();
    }
    if cfg!(windows) {
        identity.make_ascii_lowercase();
    }
    identity
}

/// FNV-1a over the normalized path bytes: stable across platforms and releases,
/// which a directory name must be.
fn cwd_hash(cwd: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in cwd_identity(cwd).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Rejects flag combinations the TypeScript CLI rejects.
fn check_flags(args: &Args) -> Result<(), String> {
    let names_a_session = args.session.is_some() || args.session_id.is_some();
    if args.no_session
        && (names_a_session || args.continue_session || args.resume || args.fork.is_some())
    {
        return Err(
            "--no-session cannot be combined with --session, --session-id, --continue, --resume, or --fork"
                .to_owned(),
        );
    }
    if args.session_id.is_some() && (args.session.is_some() || args.continue_session || args.resume)
    {
        return Err(
            "--session-id cannot be combined with --session, --continue, or --resume".to_owned(),
        );
    }
    if args.fork.is_some() && (args.continue_session || args.resume) {
        return Err("--fork cannot be combined with --continue or --resume".to_owned());
    }
    if args.resume {
        return Err(
            "--resume needs the interactive session picker, which the Rust preview does not have yet; use --continue or --session <id>"
                .to_owned(),
        );
    }
    Ok(())
}

fn describe(error: &SessionStoreError, id: &SessionId) -> String {
    match error {
        SessionStoreError::Locked { .. } => format!(
            "session \"{id}\" is in use by another Nyan process; wait for it to finish or pick another session"
        ),
        other => format!("could not open session \"{id}\": {other}"),
    }
}

/// Exports a named session without loading a provider.
///
/// # Errors
///
/// Returns a user-facing message when the session cannot be opened or the
/// destination cannot be written.
pub fn export_session(
    input: &str,
    output: Option<&str>,
    agent_dir: &Path,
    cwd: &Path,
) -> Result<PathBuf, String> {
    let args = Args {
        session: Some(input.to_owned()),
        ..Args::default()
    };
    let mut session = open_session(&args, agent_dir, cwd)?;
    let output = output.map_or_else(|| cwd.join(format!("{input}.session.jsonl")), PathBuf::from);
    let output = if output.is_absolute() {
        output
    } else {
        cwd.join(output)
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create export directory: {error}"))?;
    }
    session
        .export_jsonl(&output)
        .map_err(|error| format!("could not export session: {error}"))?;
    Ok(output)
}

/// Opens the session this invocation should use.
///
/// # Errors
///
/// Returns a user-facing message when the flags conflict, a named session does
/// not exist or is ambiguous, or the store cannot be opened.
pub fn open_session(args: &Args, agent_dir: &Path, cwd: &Path) -> Result<NativeSession, String> {
    check_flags(args)?;
    let name = args.name.as_deref().and_then(normalize_session_name);

    let ephemeral_id = |id: SessionId| SessionOpenRequest {
        descriptor: SessionDescriptor::ephemeral(id, cwd),
        mode: AgentMode::Print,
        storage_root: None,
    };
    if args.no_session {
        let id = SessionId::new(new_session_id()).map_err(|error| error.to_string())?;
        let store = FileSessionStore::new(PathBuf::new());
        let mut session = NativeSession::open(&store, ephemeral_id(id))
            .map_err(|error| format!("could not start the session: {error}"))?;
        if let Some(name) = &name {
            session.set_name(name).map_err(|error| error.to_string())?;
        }
        return Ok(session);
    }

    let base = args
        .session_dir
        .as_deref()
        .map_or_else(|| agent_dir.join(SESSION_STORE_DIR), PathBuf::from);
    let root = base.join(cwd_hash(cwd));
    let store = FileSessionStore::new(&root);
    let known = store
        .sessions_by_recency(Some(&root))
        .map_err(|error| format!("could not list sessions: {error}"))?;

    let id = if let Some(wanted) = &args.session_id {
        SessionId::new(wanted.clone()).map_err(|error| error.to_string())?
    } else if let Some(wanted) = &args.session {
        let exact = known.iter().find(|id| id.as_str() == wanted);
        if let Some(exact) = exact {
            exact.clone()
        } else {
            let mut matches = known
                .iter()
                .filter(|id| id.as_str().starts_with(wanted.as_str()));
            match (matches.next(), matches.next()) {
                (Some(only), None) => only.clone(),
                (None, _) => {
                    return Err(format!(
                        "no session matches \"{wanted}\" in {}",
                        root.display()
                    ));
                }
                (Some(_), Some(_)) => {
                    return Err(format!(
                        "\"{wanted}\" matches more than one session; give more of the id"
                    ));
                }
            }
        }
    } else if args.continue_session || args.fork.is_some() {
        match known.first() {
            Some(newest) => newest.clone(),
            None if args.fork.is_some() => {
                return Err(format!("no session to fork in {}", root.display()));
            }
            None => SessionId::new(new_session_id()).map_err(|error| error.to_string())?,
        }
    } else {
        SessionId::new(new_session_id()).map_err(|error| error.to_string())?
    };

    let request = SessionOpenRequest {
        descriptor: SessionDescriptor::persistent(id.clone(), cwd),
        mode: AgentMode::Print,
        storage_root: Some(root.clone()),
    };
    let mut session =
        NativeSession::open(&store, request).map_err(|error| describe(&error, &id))?;
    if let Some(entry) = &args.fork {
        let entry_id = SessionEntryId::parse(entry).ok_or_else(|| {
            format!(
                "invalid fork entry id \"{entry}\"; Rust-native entry ids are 16 hexadecimal characters"
            )
        })?;
        let target_id = SessionId::new(new_session_id()).map_err(|error| error.to_string())?;
        let target_request = SessionOpenRequest {
            descriptor: SessionDescriptor::persistent(target_id, cwd),
            mode: AgentMode::Print,
            storage_root: Some(root),
        };
        let mut fork = session
            .fork_at(&store, target_request, &entry_id)
            .map_err(|error| format!("could not fork session at {entry}: {error}"))?;
        let fork_name = name
            .as_deref()
            .map_or_else(|| format!("fork {entry}"), str::to_owned);
        fork.set_name(&fork_name)
            .map_err(|error| error.to_string())?;
        return Ok(fork);
    }
    if let Some(name) = &name {
        session.set_name(name).map_err(|error| error.to_string())?;
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use nyan_coding_agent_core::{Message, MessageRole};
    use nyan_coding_agent_session::SessionRecordKind;

    use super::{cwd_hash, cwd_identity, export_session, open_session};
    use crate::args::parse_args;

    fn temp(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("nyan-setup-{name}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).expect("temp dir");
        path
    }

    fn open(
        args: &[&str],
        agent: &Path,
        cwd: &Path,
    ) -> Result<nyan_coding_agent_session::NativeSession, String> {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        open_session(&parse_args(&args), agent, cwd)
    }

    fn id_of(session: &nyan_coding_agent_session::NativeSession) -> String {
        session.session().descriptor.id.as_str().to_owned()
    }

    fn say(session: &mut nyan_coding_agent_session::NativeSession, text: &str) {
        session
            .append_message(
                SessionRecordKind::UserMessage,
                Message::text(MessageRole::User, text),
            )
            .expect("append");
    }

    #[test]
    fn the_cwd_hash_is_stable_and_distinguishes_directories() {
        assert_eq!(cwd_hash(Path::new("/a/b")), cwd_hash(Path::new("/a/b")));
        assert_ne!(cwd_hash(Path::new("/a/b")), cwd_hash(Path::new("/a/c")));
        assert_eq!(
            cwd_hash(Path::new("C:\\a\\b")),
            cwd_hash(Path::new("C:/a/b"))
        );
        assert_eq!(cwd_hash(Path::new("/a/b")).len(), 16);
        assert_eq!(
            cwd_identity(Path::new(r"\\?\C:\a\b")),
            cwd_identity(Path::new("C:/a/b"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_directory_and_its_symlink_have_one_cwd_identity() {
        use std::os::unix::fs::symlink;

        let root = temp("cwd-identity");
        let real = root.join("real");
        let link = root.join("link");
        std::fs::create_dir(&real).expect("real");
        symlink(&real, &link).expect("link");
        assert_eq!(cwd_identity(&real), cwd_identity(&link));
        assert_eq!(cwd_hash(&real), cwd_hash(&link));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_new_session_is_saved_and_no_session_writes_nothing() {
        let root = temp("save");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let mut saved = open(&[], &agent, &cwd).expect("open");
        say(&mut saved, "kept");
        assert!(agent.join(super::SESSION_STORE_DIR).is_dir());
        drop(saved);

        let other_agent = root.join("agent2");
        let mut ephemeral = open(&["--no-session"], &other_agent, &cwd).expect("open");
        say(&mut ephemeral, "forgotten");
        assert!(
            !other_agent.exists(),
            "an ephemeral session leaves nothing behind"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn continue_reopens_the_newest_session_of_this_directory_only() {
        let root = temp("continue");
        let agent = root.join("agent");
        let here = root.join("here");
        let elsewhere = root.join("elsewhere");
        let mut first = open(&[], &agent, &here).expect("open");
        say(&mut first, "from here");
        let first_id = id_of(&first);
        drop(first);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let mut other = open(&[], &agent, &elsewhere).expect("open");
        say(&mut other, "from elsewhere");
        drop(other);

        let continued = open(&["--continue"], &agent, &here).expect("continue");
        assert_eq!(id_of(&continued), first_id);
        assert_eq!(continued.messages()[0].text, "from here");
        drop(continued);
        let fresh = open(&["--continue"], &agent, &root.join("never-used")).expect("fresh");
        assert!(
            fresh.messages().is_empty(),
            "a directory without sessions starts a new one"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn session_id_opens_or_creates_that_exact_id_and_session_accepts_a_unique_prefix() {
        let root = temp("ids");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let mut created = open(&["--session-id", "alpha-one"], &agent, &cwd).expect("create");
        say(&mut created, "hello");
        drop(created);
        let mut other = open(&["--session-id", "beta"], &agent, &cwd).expect("create");
        say(&mut other, "other");
        drop(other);

        let reopened = open(&["--session-id", "alpha-one"], &agent, &cwd).expect("reopen");
        assert_eq!(reopened.messages().len(), 1);
        drop(reopened);
        let by_prefix = open(&["--session", "alp"], &agent, &cwd).expect("prefix");
        assert_eq!(id_of(&by_prefix), "alpha-one");
        drop(by_prefix);
        assert!(
            open(&["--session", "zzz"], &agent, &cwd)
                .expect_err("none")
                .contains("no session matches")
        );
        let mut third = open(&["--session-id", "alpha-two"], &agent, &cwd).expect("create");
        say(&mut third, "x");
        drop(third);
        assert!(
            open(&["--session", "alpha"], &agent, &cwd)
                .expect_err("ambiguous")
                .contains("more than one")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn fork_opens_a_new_session_at_a_user_entry() {
        let root = temp("fork");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let mut source = open(&["--session-id", "source"], &agent, &cwd).expect("source");
        say(&mut source, "first");
        source
            .append_message(
                SessionRecordKind::AssistantMessage,
                Message::text(MessageRole::Assistant, "answer"),
            )
            .expect("assistant");
        let entry = source.get_entries().expect("entries")[0].id.to_string();
        drop(source);

        let fork = open(&["--session-id", "source", "--fork", &entry], &agent, &cwd).expect("fork");
        assert_ne!(id_of(&fork), "source");
        assert_eq!(fork.messages().len(), 1);
        assert_eq!(fork.messages()[0].text, "first");
        let expected_name = format!("fork {entry}");
        assert_eq!(fork.name(), Some(expected_name.as_str()));
        drop(fork);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn export_writes_a_session_without_loading_a_provider() {
        let root = temp("export");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let mut source = open(&["--session-id", "source"], &agent, &cwd).expect("source");
        say(&mut source, "hello");
        drop(source);
        let output =
            export_session("source", Some("out/session.jsonl"), &agent, &cwd).expect("export");
        assert_eq!(output, cwd.join("out/session.jsonl"));
        assert!(
            std::fs::read_to_string(output)
                .expect("export file")
                .contains("nyan_session_export")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn session_dir_moves_the_store() {
        let root = temp("dir");
        let custom = root.join("custom");
        let mut session = open(
            &["--session-dir", custom.to_str().expect("utf8")],
            &root.join("agent"),
            &root.join("project"),
        )
        .expect("open");
        say(&mut session, "x");
        assert!(custom.is_dir());
        assert!(!root.join("agent").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_name_flag_is_trimmed_and_saved_and_a_blank_name_is_ignored() {
        let root = temp("name");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let named = open(
            &["--session-id", "n", "--name", "  Refactor auth  "],
            &agent,
            &cwd,
        )
        .expect("open");
        assert_eq!(named.name(), Some("Refactor auth"));
        drop(named);
        let again = open(&["--session-id", "n"], &agent, &cwd).expect("reopen");
        assert_eq!(again.name(), Some("Refactor auth"));
        drop(again);
        let blank = open(&["--session-id", "b", "--name", "   "], &agent, &cwd).expect("open");
        assert_eq!(blank.name(), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn conflicting_and_unsupported_flags_are_refused_with_a_reason() {
        let root = temp("flags");
        let agent = root.join("agent");
        let cwd = root.join("project");
        for (args, message) in [
            (
                &["--no-session", "--continue"][..],
                "--no-session cannot be combined",
            ),
            (
                &["--session-id", "a", "--continue"][..],
                "--session-id cannot be combined",
            ),
            (&["--fork", "e1"][..], "no session to fork"),
            (
                &["--resume"][..],
                "--resume needs the interactive session picker",
            ),
        ] {
            let error = open(args, &agent, &cwd)
                .err()
                .unwrap_or_else(|| panic!("{args:?} must be refused"));
            assert!(error.contains(message), "{args:?}: {error}");
        }
        assert!(!agent.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_session_held_by_another_process_is_reported_plainly() {
        let root = temp("locked");
        let agent = root.join("agent");
        let cwd = root.join("project");
        let held = open(&["--session-id", "busy"], &agent, &cwd).expect("open");
        let error = open(&["--session-id", "busy"], &agent, &cwd).expect_err("locked");
        assert_eq!(
            error,
            "session \"busy\" is in use by another Nyan process; wait for it to finish or pick another session"
        );
        drop(held);
        let _ = std::fs::remove_dir_all(root);
    }
}
