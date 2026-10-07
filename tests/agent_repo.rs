#![cfg(feature = "test-support")]
use enfour_memory::{
    agent_repo::{self, Files},
    engine::Engine,
    git_memory::Mirror,
    store::{Relation, Remember},
};
use std::{fs, process::Command};
fn files(items: &[(&str, &str)]) -> Files {
    Files {
        files: items
            .iter()
            .map(|(p, t)| (p.to_string(), t.to_string()))
            .collect(),
    }
}
fn note(key: &str) -> Remember {
    Remember {
        scope: "repo:test/agent".into(),
        key: key.into(),
        title: "Local storage".into(),
        content: "Use SQLite for local memory.\n\n```text\nexact; NOT prose\n```".into(),
        kind: "decision".into(),
        source: "test://source; exact]".into(),
        expected_revision: 0,
        expires_at: None,
    }
}
#[test]
fn format_contract_and_extensions() {
    let valid = files(&[
        ("MEMORY.md", "# Memory\n\n## Index\n- [[notes/storage]]\n"),
        (
            "notes/storage.md",
            "# Storage\n\n- Use SQLite. [source: test://one; added: 2024-02-29; custom: value]\n- [[query.sql]]\n",
        ),
        ("query.sql", "select 1; -- [[not-a-link]]"),
    ]);
    assert!(agent_repo::validate(&valid).accepted);
    for (path, content, rule) in [
        (
            "notes/storage.md",
            "- Use SQLite. [added: 2025-02-29]\n",
            "metadata.date",
        ),
        (
            "notes/storage.md",
            "- Use SQLite. [source: ]\n",
            "metadata.syntax",
        ),
        ("MEMORY.md", "# Memory\n- [[notes/storage]]\n", "root.index"),
        (
            "MEMORY.md",
            "## Index\n- [[notes/storage.md]]\n",
            "links.markdown_extension",
        ),
        ("MEMORY.md", "## Index\n- [[../outside]]\n", "links.path"),
        ("MEMORY.md", "## Index\n- [[missing]]\n", "links.missing"),
        (
            "notes/storage.md",
            "- A long entry\n  continues here.\n",
            "entries.single_line",
        ),
    ] {
        let mut bad = valid.clone();
        bad.files.insert(path.into(), content.into());
        let report = agent_repo::validate(&bad);
        assert!(!report.accepted, "{rule}");
        assert!(report.findings.iter().any(|f| f.rule == rule), "{report:?}");
    }
    let optional = files(&[("MEMORY.md", "## Index\n- Use SQLite.\n")]);
    let report = agent_repo::validate(&optional);
    assert!(report.accepted);
    assert!(report.findings.iter().any(|f| f.rule == "enfour.source"));
    let code = files(&[("MEMORY.md", "## Index\n- Keep `[[literal]]` unchanged.\n")]);
    assert!(agent_repo::validate(&code).accepted);
}
#[test]
fn reject_unsafe_names_without_touching_files() {
    for path in [
        "/etc/passwd",
        "../outside",
        "a/../b",
        "a\\b",
        ".git/config",
        "A/.GIT/hooks/a",
        "x:y",
        "a//b",
    ] {
        assert!(!agent_repo::safe_path(path), "{path}");
    }
    let mut case = files(&[("MEMORY.md", "## Index\n"), ("memory.md", "## Index\n")]);
    assert!(!agent_repo::validate(&case).accepted);
    case.files.remove("memory.md");
    case.files.insert("a".into(), "".into());
    case.files.insert("a/b.txt".into(), "".into());
    assert!(!agent_repo::validate(&case).accepted);
}
#[test]
fn export_import_preserves_exact_content_and_revision_checks() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open_test(&dir.path().join("db"), None).unwrap();
    let original = engine.remember(note("storage")).unwrap();
    let view = agent_repo::snapshot(&engine.store, &original.scope).unwrap();
    assert!(agent_repo::validate(&view).accepted);
    assert_eq!(
        view.files[&format!("content/{}.txt", original.id)],
        original.content
    );
    let metadata: serde_json::Value =
        serde_json::from_str(&view.files[&format!("records/{}.json", original.id)]).unwrap();
    let request = agent_repo::import_request(
        &engine.store,
        &original.scope,
        metadata.clone(),
        original.content.clone(),
        1,
    )
    .unwrap();
    let revised = engine.remember(request).unwrap();
    assert_eq!(revised.id, original.id);
    assert_eq!(revised.revision, 2);
    assert_eq!(revised.source, original.source);
    assert!(
        agent_repo::import_request(
            &engine.store,
            "repo:other",
            metadata.clone(),
            original.content.clone(),
            1
        )
        .is_err()
    );
    let stale = agent_repo::import_request(
        &engine.store,
        &original.scope,
        metadata,
        original.content.clone(),
        1,
    )
    .unwrap();
    assert!(engine.remember(stale).is_err());
    assert_eq!(
        engine
            .store
            .history(&original.scope, &original.id)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        agent_repo::snapshot(&engine.store, "repo:other")
            .unwrap()
            .files
            .len(),
        1
    );
}
#[test]
fn durable_events_and_private_git_recover_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open_test(&dir.path().join("db"), None).unwrap();
    let a = engine.remember(note("storage")).unwrap();
    let b = engine.remember(note("second")).unwrap();
    engine
        .store
        .relate(Relation {
            scope: a.scope.clone(),
            from: a.id.clone(),
            to: b.id.clone(),
            from_revision: 1,
            to_revision: 1,
            kind: "supports".into(),
            source: "test://link".into(),
        })
        .unwrap();
    let count = || {
        engine
            .store
            .db
            .query_row("SELECT count(*) FROM agent_git_events", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    let initial = count();
    assert_eq!(initial, 3);
    let mut invalid = note("invalid");
    invalid.content = "This can't pass.".into();
    assert!(engine.remember(invalid).is_err());
    assert_eq!(
        engine
            .store
            .db
            .query_row("SELECT count(*) FROM agent_git_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        initial
    );
    let root = dir.path().join(".agent-memory");
    let scope_root = root.join(enfour_memory::models::digest_hex(a.scope.as_bytes()));
    {
        let mut mirror = Mirror::open(&root).unwrap();
        let status = mirror.sync(&engine.store.db).unwrap();
        assert_eq!(status.applied_event, status.queued_event);
        assert!(Mirror::open(&root).is_err());
    }
    assert!(scope_root.join(".git").is_dir());
    let command = |args: &[&str]| {
        let o = Command::new("git")
            .arg("-C")
            .arg(&scope_root)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap()
    };
    assert_eq!(command(&["rev-list", "--count", "HEAD"]).trim(), "3");
    assert_eq!(
        fs::read_to_string(scope_root.join(format!("content/{}.txt", a.id))).unwrap(),
        a.content
    );
    assert!(
        fs::read_to_string(scope_root.join(format!("notes/{}.md", a.id)))
            .unwrap()
            .contains(&format!("[[notes/{}]]", b.id))
    );
    fs::write(scope_root.join("unfinished.txt"), "not committed").unwrap();
    {
        let mut mirror = Mirror::open(&root).unwrap();
        mirror.sync(&engine.store.db).unwrap();
    }
    assert!(!scope_root.join("unfinished.txt").exists());
    assert_eq!(command(&["rev-list", "--count", "HEAD"]).trim(), "3");
    engine.store.forget(&a.scope, &a.id, 1).unwrap();
    let mut mirror = Mirror::open(&root).unwrap();
    mirror.sync(&engine.store.db).unwrap();
    assert!(!scope_root.join(format!("content/{}.txt", a.id)).exists());
    assert_eq!(command(&["rev-list", "--count", "HEAD"]).trim(), "4");
    assert_eq!(engine.store.history(&a.scope, &a.id).unwrap().len(), 2);
    assert!(command(&["status", "--porcelain"]).is_empty());
}
#[test]
fn git_failure_does_not_lose_an_accepted_memory() {
    let dir = tempfile::tempdir().unwrap();
    let mut e = Engine::open_test(&dir.path().join("db"), None).unwrap();
    let m = e.remember(note("storage")).unwrap();
    let root = dir.path().join(".agent-memory");
    let scope = root.join(enfour_memory::models::digest_hex(m.scope.as_bytes()));
    fs::create_dir_all(&scope).unwrap();
    fs::write(scope.join("user-file"), "keep").unwrap();
    let mut mirror = Mirror::open(&root).unwrap();
    assert!(mirror.sync(&e.store.db).is_err());
    assert_eq!(e.store.get(&m.scope, &m.id).unwrap().content, m.content);
    assert_eq!(fs::read_to_string(scope.join("user-file")).unwrap(), "keep");
    fs::remove_file(scope.join("user-file")).unwrap();
    fs::remove_dir(&scope).unwrap();
    let staging = scope.with_extension("init");
    fs::create_dir_all(staging.join(".git")).unwrap();
    fs::write(staging.join("enfour-init-owner"), "enfour-private-git-v1").unwrap();
    let status = mirror.sync(&e.store.db).unwrap();
    assert_eq!(status.applied_event, status.queued_event);
}

#[test]
fn interrupted_git_update_and_database_restore_are_detected() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("db");
    let mut engine = Engine::open_test(&db, None).unwrap();
    let a = engine.remember(note("storage")).unwrap();
    let backup = dir.path().join("before.sqlite");
    engine.store.backup(&backup).unwrap();
    let root = dir.path().join(".agent-memory");
    let repo = root.join(enfour_memory::models::digest_hex(a.scope.as_bytes()));
    let mut mirror = Mirror::open(&root).unwrap();
    mirror.sync(&engine.store.db).unwrap();
    let mut update = note("storage");
    update.expected_revision = 1;
    engine.remember(update).unwrap();
    fs::write(repo.join(".git/index.lock"), "blocked").unwrap();
    assert!(mirror.sync(&engine.store.db).is_err());
    fs::remove_file(repo.join(".git/index.lock")).unwrap();
    assert_eq!(mirror.sync(&engine.store.db).unwrap().applied_event, 2);
    drop(mirror);
    let earlier = rusqlite::Connection::open(backup).unwrap();
    let mut mirror = Mirror::open(&root).unwrap();
    assert!(
        mirror
            .sync(&earlier)
            .unwrap_err()
            .to_string()
            .contains("correct backup")
    );
    assert_eq!(engine.store.get(&a.scope, &a.id).unwrap().revision, 2);
}
#[test]
fn expiry_removes_current_git_files_without_losing_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::open_test(&dir.path().join("db"), None).unwrap();
    assert!(engine.store.validate_memory(&note("expiry")).accepted);
    let mut request = note("expiry");
    request.expires_at = Some(enfour_memory::store::now() + 4);
    let memory = engine.remember(request).unwrap();
    let root = dir.path().join(".agent-memory");
    let repo = root.join(enfour_memory::models::digest_hex(memory.scope.as_bytes()));
    let mut mirror = Mirror::open(&root).unwrap();
    mirror.sync(&engine.store.db).unwrap();
    std::thread::sleep(std::time::Duration::from_secs(4));
    mirror.sync(&engine.store.db).unwrap();
    assert!(!repo.join(format!("content/{}.txt", memory.id)).exists());
    assert!(
        repo.join(format!(".enfour/records/{}.json", memory.id))
            .exists()
    );
    assert_eq!(
        engine
            .store
            .history(&memory.scope, &memory.id)
            .unwrap()
            .len(),
        1
    );
}
