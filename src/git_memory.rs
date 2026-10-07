//! Private Git history fed by a transactional SQLite outbox. Never a project checkout.
use crate::{
    agent_repo,
    store::{Memory, Relation},
};
use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
const OWNER: &str = "enfour-private-git-v1";

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub applied_event: i64,
    pub queued_event: i64,
    pub error: Option<String>,
}
struct Repository {
    root: PathBuf,
    cursor: i64,
    event_hash: String,
    records: BTreeMap<String, Memory>,
    relations: BTreeMap<String, Relation>,
    next_expiry: Option<i64>,
}
fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "gc.auto=0",
            "-c",
            "core.fsync=all",
            "-c",
            "user.name=Enfour Memory",
            "-c",
            "user.email=memory@enfour.invalid",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .context("Git could not start.")?;
    ensure!(
        output.status.success(),
        "Git failed.\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}
fn write(root: &Path, path: &str, bytes: &[u8]) -> Result<()> {
    ensure!(agent_repo::safe_path(path), "The Git path is invalid.");
    let target = root.join(path);
    let parent = target.parent().unwrap();
    // Owned worktrees contain only generated files. Do not follow an injected symlink.
    let mut current = root.to_path_buf();
    for part in Path::new(path).components() {
        current.push(part);
        if let Ok(metadata) = fs::symlink_metadata(&current) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "The Git contains a file link."
            );
        }
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    if fs::read(&target).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
impl Repository {
    fn open(root: PathBuf) -> Result<Self> {
        if root.exists() {
            ensure!(
                !fs::symlink_metadata(&root)?.file_type().is_symlink(),
                "The Git root must not be a file link."
            );
            ensure!(
                fs::read_to_string(root.join(".git/enfour-owner")).is_ok_and(|s| s == OWNER),
                "The Git directory is not an Enfour directory."
            );
            ensure!(
                !fs::symlink_metadata(root.join(".git"))?
                    .file_type()
                    .is_symlink(),
                "The Git directory must not be a file link."
            );
        } else {
            // Finish initialization in a private staging directory, then publish it atomically.
            let staging = root.with_extension("init");
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&staging)?;
            ensure!(
                !fs::symlink_metadata(&staging)?.file_type().is_symlink(),
                "The temporary Git directory must not be a file link."
            );
            let marker = staging.join("enfour-init-owner");
            if fs::read_dir(&staging)?.next().is_none() {
                fs::write(&marker, OWNER)?;
            }
            ensure!(
                fs::read_to_string(&marker).is_ok_and(|s| s == OWNER)
                    || fs::read_to_string(staging.join(".git/enfour-owner"))
                        .is_ok_and(|s| s == OWNER),
                "The temporary Git directory is not an Enfour directory."
            );
            if let Ok(metadata) = fs::symlink_metadata(staging.join(".git")) {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "The Git directory must not be a file link."
                );
            }
            git(
                &staging,
                &["init", "--quiet", "--template=", "--initial-branch=memory"],
            )?;
            fs::write(staging.join(".git/enfour-owner"), OWNER)?;
            if marker.exists() {
                fs::remove_file(marker)?;
            }
            fs::rename(&staging, &root)?;
            fs::File::open(root.parent().unwrap())?.sync_all()?;
        }
        let mut repo = Self {
            root,
            cursor: 0,
            event_hash: String::new(),
            records: BTreeMap::new(),
            relations: BTreeMap::new(),
            next_expiry: None,
        };
        // Discard only unfinished generated work in this verified, service-owned repository.
        if git(&repo.root, &["rev-parse", "--verify", "HEAD"]).is_ok() {
            git(&repo.root, &["reset", "--hard", "HEAD"])?;
            git(&repo.root, &["clean", "-fd"])?;
            repo.cursor = fs::read_to_string(repo.root.join(".enfour/cursor"))?
                .trim()
                .parse()?;
            repo.event_hash = fs::read_to_string(repo.root.join(".enfour/event-hash"))?;
            for (folder, records) in [("records", true), ("relations", false)] {
                let directory = repo.root.join(".enfour").join(folder);
                if !directory.exists() {
                    continue;
                }
                for entry in fs::read_dir(directory)? {
                    let path = entry?.path();
                    ensure!(
                        path.is_file() && !fs::symlink_metadata(&path)?.file_type().is_symlink(),
                        "The Git state file is invalid."
                    );
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .context("The Git ID is invalid.")?
                        .to_string();
                    let bytes = fs::read(path)?;
                    if records {
                        repo.records.insert(id, serde_json::from_slice(&bytes)?);
                    } else {
                        repo.relations.insert(id, serde_json::from_slice(&bytes)?);
                    }
                }
            }
        } else {
            // No commit exists yet. A prior process can have stopped during its first write.
            git(&repo.root, &["read-tree", "--empty"])?;
            git(&repo.root, &["clean", "-fd"])?;
        }
        repo.next_expiry = repo
            .records
            .values()
            .filter(|m| !m.deleted)
            .filter_map(|m| m.expires_at)
            .min();
        Ok(repo)
    }
    fn materialize(&self) -> Result<agent_repo::Files> {
        agent_repo::materialize(
            self.records.values().cloned().collect(),
            &self.relations.values().cloned().collect::<Vec<_>>(),
        )
    }
    fn publish(&mut self) -> Result<()> {
        let new = self.materialize()?;
        for path in git(&self.root, &["ls-files", "-z"])?
            .split('\0')
            .filter(|p| !p.is_empty() && !p.starts_with(".enfour/"))
        {
            ensure!(
                agent_repo::safe_path(path),
                "The stored Git path is invalid."
            );
            if !new.files.contains_key(path) {
                match fs::remove_file(self.root.join(path)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                }
            }
        }
        for (path, text) in &new.files {
            write(&self.root, path, text.as_bytes())?;
        }
        let at = crate::store::now();
        self.next_expiry = self
            .records
            .values()
            .filter(|m| !m.deleted)
            .filter_map(|m| m.expires_at)
            .filter(|expiry| *expiry > at)
            .min();
        Ok(())
    }
    fn expire(&mut self) -> Result<()> {
        if self
            .next_expiry
            .is_some_and(|expiry| expiry <= crate::store::now())
        {
            self.publish()?;
            git(&self.root, &["add", "--all", "--force", "--", "."])?;
            if !git(&self.root, &["status", "--porcelain"])?.is_empty() {
                git(
                    &self.root,
                    &[
                        "commit",
                        "--quiet",
                        "-m",
                        "Remove expired entries from the current view",
                    ],
                )?;
            }
        }
        Ok(())
    }
    fn apply(&mut self, seq: i64, kind: &str, id: &str, data: &str) -> Result<()> {
        if seq <= self.cursor {
            return Ok(());
        }
        ensure!(
            id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') && !id.is_empty(),
            "The event ID is invalid."
        );
        match kind {
            "memory" => {
                self.records.insert(id.into(), serde_json::from_str(data)?);
            }
            "relation" => {
                self.relations
                    .insert(id.into(), serde_json::from_str(data)?);
            }
            "remove_relation" => {
                self.relations.remove(id);
            }
            _ => anyhow::bail!("The Git event kind is not supported."),
        }
        self.publish()?;
        let path = format!(
            ".enfour/{}/{id}.json",
            if kind == "memory" {
                "records"
            } else {
                "relations"
            }
        );
        if kind == "remove_relation" {
            match fs::remove_file(self.root.join(&path)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        } else {
            write(&self.root, &path, data.as_bytes())?;
        }
        write(&self.root, ".enfour/cursor", seq.to_string().as_bytes())?;
        let event_hash = crate::models::digest_hex(format!("{kind}\0{id}\0{data}").as_bytes());
        write(&self.root, ".enfour/event-hash", event_hash.as_bytes())?;
        git(&self.root, &["add", "--all", "--force", "--", "."])?;
        git(
            &self.root,
            &[
                "commit",
                "--quiet",
                "-m",
                &format!("Record memory event {seq}"),
            ],
        )?;
        self.cursor = seq;
        self.event_hash = event_hash;
        Ok(())
    }
}
pub struct Mirror {
    root: PathBuf,
    cursor: i64,
    repositories: BTreeMap<String, Repository>,
    _lock: fs::File,
}
impl Mirror {
    pub fn open(root: &Path) -> Result<Self> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)?;
        ensure!(
            !fs::symlink_metadata(root)?.file_type().is_symlink(),
            "The Git root must not be a file link."
        );
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(root.join(".lock"))?;
        lock.try_lock()
            .context("A Git process uses this directory.")?;
        Ok(Self {
            root: root.into(),
            cursor: 0,
            repositories: BTreeMap::new(),
            _lock: lock,
        })
    }
    pub fn sync(&mut self, db: &Connection) -> Result<Status> {
        let maximum: i64 = db.query_row(
            "SELECT coalesce(max(seq),0) FROM agent_git_events",
            [],
            |r| r.get(0),
        )?;
        let events: Vec<(i64,String,String,String,String)> = db.prepare("SELECT seq,scope,kind,object_id,data FROM agent_git_events WHERE seq>? ORDER BY seq LIMIT 64")?
            .query_map([self.cursor], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?.collect::<std::result::Result<_,_>>()?;
        for (seq, scope, kind, id, data) in events {
            let folder = crate::models::digest_hex(scope.as_bytes());
            if !self.repositories.contains_key(&scope) {
                let repository = Repository::open(self.root.join(folder))?;
                if repository.cursor > 0 {
                    let stored: Option<(String, String, String, String)> = {
                        use rusqlite::OptionalExtension;
                        db.query_row(
                            "SELECT scope,kind,object_id,data FROM agent_git_events WHERE seq=?",
                            [repository.cursor],
                            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                        )
                        .optional()?
                    };
                    let valid = stored.is_some_and(|(saved_scope, kind, id, data)| {
                        saved_scope == scope
                            && crate::models::digest_hex(format!("{kind}\0{id}\0{data}").as_bytes())
                                == repository.event_hash
                    });
                    ensure!(
                        valid,
                        "Git history and this database are different. Restore the correct backup."
                    );
                }
                self.repositories.insert(scope.clone(), repository);
            }
            let repository = self.repositories.get_mut(&scope).unwrap();
            if let Err(error) = repository.apply(seq, &kind, &id, &data) {
                // Reload committed state before retry. Never acknowledge an incomplete commit.
                self.repositories.remove(&scope);
                return Err(error);
            }
            self.cursor = seq;
        }
        for scope in self.repositories.keys().cloned().collect::<Vec<_>>() {
            if let Err(error) = self.repositories.get_mut(&scope).unwrap().expire() {
                self.repositories.remove(&scope);
                return Err(error);
            }
        }
        Ok(Status {
            applied_event: self.cursor,
            queued_event: maximum,
            error: None,
        })
    }
}
pub fn start(db: PathBuf, root: PathBuf, stop: CancellationToken) -> Arc<Mutex<Status>> {
    let status = Arc::new(Mutex::new(Status::default()));
    let shared = status.clone();
    tokio::task::spawn_blocking(move || {
        let mut mirror = None;
        while !stop.is_cancelled() {
            let result = (|| {
                if mirror.is_none() {
                    mirror = Some(Mirror::open(&root)?);
                }
                let db =
                    Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                db.busy_timeout(Duration::from_secs(5))?;
                mirror.as_mut().unwrap().sync(&db)
            })();
            let busy = match result {
                Ok(value) => {
                    let busy = value.applied_event < value.queued_event;
                    *shared.lock().unwrap() = value;
                    busy
                }
                Err(error) => {
                    shared.lock().unwrap().error = Some(format!("{error:#}"));
                    false
                }
            };
            if !busy {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    });
    status
}
