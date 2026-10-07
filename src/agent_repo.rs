//! Portable Agent Memory Repo views. SQLite remains authoritative.
use crate::store::{Memory, Store};
use anyhow::{Result, ensure};
use pulldown_cmark::{Event, Parser, Tag, TagEnd};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const PROFILE: &str = "enfour-agent-repo-v1";
pub const SPEC: &str = "https://github.com/AgentMemoryRepo/agentmemoryrepo/blob/8798cb26c817d451a5ed6cba0ad8d9eca8d5ec3e/SPEC.md";
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILES: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Files {
    pub files: BTreeMap<String, String>,
}
#[derive(Debug, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    pub path: String,
    pub line: usize,
    pub severity: &'static str,
    pub guidance: &'static str,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub accepted: bool,
    pub spec: &'static str,
    pub profile: &'static str,
    pub findings: Vec<Finding>,
    pub review: &'static str,
}
impl Report {
    fn add(
        &mut self,
        path: &str,
        line: usize,
        rule: &'static str,
        severity: &'static str,
        guidance: &'static str,
    ) {
        self.accepted &= severity != "error";
        self.findings.push(Finding {
            rule,
            path: path.into(),
            line,
            severity,
            guidance,
        });
    }
}
pub fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 512
        && !path.contains(['\\', ':', '\0'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && !p.eq_ignore_ascii_case(".git"))
}
fn date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = value[..4].parse().unwrap_or(0);
    let month: usize = value[5..7].parse().unwrap_or(0);
    let day: u32 = value[8..].parse().unwrap_or(0);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        0,
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    year > 0 && month <= 12 && day > 0 && day <= days[month]
}
fn metadata(entry: &str, path: &str, line: usize, report: &mut Report) -> bool {
    let mut rest = entry.trim_end();
    let mut source = false;
    let mut keys = BTreeSet::new();
    while rest.ends_with(']') && !rest.ends_with("]]") {
        let Some(start) = rest.rfind('[') else { break };
        let data = &rest[start + 1..rest.len() - 1];
        if !data.contains(':') {
            break;
        }
        for field in data.split(';') {
            match field.split_once(':') {
                Some((key, value)) if !key.trim().is_empty() && !value.trim().is_empty() => {
                    let (key, value) = (key.trim(), value.trim());
                    if !keys.insert(key) {
                        report.add(
                            path,
                            line,
                            "metadata.duplicate",
                            "advisory",
                            "Review repeated metadata keys.",
                        );
                    }
                    source |= key == "source";
                    if key == "added" && !date(value) {
                        report.add(
                            path,
                            line,
                            "metadata.date",
                            "error",
                            "Use a valid YYYY-MM-DD date.",
                        );
                    }
                }
                _ => report.add(
                    path,
                    line,
                    "metadata.syntax",
                    "error",
                    "Use `key: value` pairs with semicolons.",
                ),
            }
        }
        rest = rest[..start].trim_end();
    }
    source
}

/// Format diagnostics are independent of Enfour's mandatory prose validation.
pub fn validate(input: &Files) -> Report {
    validate_inner(input, true)
}
fn validate_inner(input: &Files, bounded: bool) -> Report {
    let mut report = Report {
        accepted: true,
        spec: SPEC,
        profile: PROFILE,
        findings: Vec::new(),
        review: "Checks cover file structure. Review facts, sources, meaning, and writing separately. Git state must be checked by the local CLI.",
    };
    if bounded
        && (input.files.len() > MAX_FILES
            || input
                .files
                .iter()
                .map(|(p, t)| p.len() + t.len())
                .sum::<usize>()
                > MAX_BYTES)
    {
        report.add(
            "MEMORY.md",
            1,
            "enfour.budget",
            "error",
            "The repository is above the file or byte limit.",
        );
        return report;
    }
    if !input.files.contains_key("MEMORY.md") {
        report.add(
            "MEMORY.md",
            1,
            "root.entrypoint",
            "error",
            "Add MEMORY.md at the memory root.",
        );
    }
    let mut folded = BTreeSet::new();
    for path in input.files.keys() {
        if !safe_path(path) || !folded.insert(path.to_lowercase()) {
            report.add(
                path,
                1,
                "enfour.path",
                "error",
                "Use a different path for each file in the memory root.",
            );
        }
        if path
            .split('/')
            .scan(String::new(), |parent, part| {
                if !parent.is_empty() {
                    parent.push('/');
                }
                parent.push_str(part);
                Some(parent.clone())
            })
            .any(|parent| parent != *path && input.files.contains_key(&parent))
        {
            report.add(
                path,
                1,
                "enfour.path_conflict",
                "error",
                "A file cannot also be a directory.",
            );
        }
    }
    let mut edges: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (path, text) in &input.files {
        if !path.ends_with(".md") {
            continue;
        }
        if text.contains('\0') {
            report.add(
                path,
                1,
                "enfour.text",
                "error",
                "Markdown must not contain NUL.",
            );
            continue;
        }
        let view = crate::language::text::View::new(text);
        let mut links = Vec::new();
        let mut cursor = 0;
        while let Some(offset) = text[cursor..].find("[[") {
            let start = cursor + offset;
            cursor = start + 2;
            if view.protected.iter().any(|r| r.contains(&start)) {
                continue;
            }
            let line = text[..start].bytes().filter(|b| *b == b'\n').count() + 1;
            let Some(end) = text[cursor..].find("]]") else {
                report.add(
                    path,
                    line,
                    "links.syntax",
                    "error",
                    "Close the file link with `]]`.",
                );
                break;
            };
            let target = &text[cursor..cursor + end];
            cursor += end + 2;
            if !safe_path(target) || target.contains(['[', ']', '|', '#', '?']) {
                report.add(
                    path,
                    line,
                    "links.path",
                    "error",
                    "Use a path from the memory root without a fragment or alias.",
                );
                continue;
            }
            if target.ends_with(".md") {
                report.add(
                    path,
                    line,
                    "links.markdown_extension",
                    "error",
                    "Remove `.md` from Markdown file links.",
                );
            }
            let file = if input.files.contains_key(&format!("{target}.md")) {
                format!("{target}.md")
            } else {
                target.into()
            };
            if !input.files.contains_key(&file) {
                report.add(
                    path,
                    line,
                    "links.missing",
                    "error",
                    "The file at the link is missing.",
                );
            } else {
                links.push(file);
            }
        }
        edges.insert(path, links);
        let mut in_item = 0usize;
        let mut has_index = false;
        for (event, range) in Parser::new(text).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { .. }) if path == "MEMORY.md" => {
                    has_index |= text[range].trim_end() == "## Index";
                }
                Event::Start(Tag::Item) => in_item += 1,
                Event::End(TagEnd::Item) => in_item = in_item.saturating_sub(1),
                Event::SoftBreak | Event::HardBreak if in_item > 0 => {
                    let line = text[..range.start].bytes().filter(|b| *b == b'\n').count() + 1;
                    report.add(
                        path,
                        line,
                        "entries.single_line",
                        "error",
                        "Keep each entry on one bullet line.",
                    );
                }
                Event::Start(Tag::Paragraph) => {
                    let content = text[range.clone()].trim_end();
                    let line = text[..range.start].bytes().filter(|b| *b == b'\n').count() + 1;
                    if in_item == 0 || content.contains('\n') {
                        report.add(
                            path,
                            line,
                            "entries.single_line",
                            "error",
                            "Keep each entry on one bullet line.",
                        );
                    }
                }
                _ => {}
            }
        }
        // Tight Markdown lists omit Paragraph events; inspect their physical entry lines too.
        let mut offset = 0;
        for (index, line) in text.split_inclusive('\n').enumerate() {
            let start = offset;
            offset += line.len();
            let trimmed = line.trim_start();
            if let Some(entry) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("+ "))
            {
                if view.protected.iter().any(|r| r.contains(&start)) {
                    continue;
                }
                let sourced = metadata(entry, path, index + 1, &mut report);
                if !sourced && !entry.trim().starts_with("[[") {
                    report.add(path,index+1,"enfour.source","advisory","Enfour writes must have a source. The repository format makes metadata optional.");
                }
            }
        }
        if path == "MEMORY.md" {
            if !has_index {
                report.add(
                    path,
                    1,
                    "root.index",
                    "error",
                    "Add an Index heading at level two.",
                );
            }
            if text.len() > 4096 {
                report.add(
                    path,
                    1,
                    "enfour.entry_budget",
                    "advisory",
                    "Keep the entry point short. Move content to other files.",
                );
            }
        }
    }
    let mut visited = BTreeSet::new();
    let mut pending = vec!["MEMORY.md".to_string()];
    while let Some(path) = pending.pop() {
        if visited.insert(path.clone())
            && let Some(links) = edges.get(path.as_str())
        {
            pending.extend(links.clone());
        }
    }
    for path in input.files.keys().filter(|p| !visited.contains(*p)) {
        report.add(
            path,
            1,
            "enfour.unreachable",
            "advisory",
            "Add a link to this file in MEMORY.md or a note.",
        );
    }
    report
}
fn markdown(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if "\\`*_{}[]<>()#!|".contains(c) {
                vec!['\\', c]
            } else if c.is_control() {
                vec![' ']
            } else {
                vec![c]
            }
        })
        .collect()
}

/// A consistent read, with no model calls. Full source content stays byte-for-byte in text assets.
pub fn snapshot(store: &Store, scope: &str) -> Result<Files> {
    let tx = store.db.unchecked_transaction()?;
    let graph = store.graph(scope)?;
    let memories: Vec<Memory> = serde_json::from_value(graph["nodes"].clone())?;
    let relations = graph["edges"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|edge| serde_json::from_value(edge["relation"].clone()))
        .collect::<std::result::Result<Vec<crate::store::Relation>, _>>()?;
    tx.commit()?;
    materialize(memories, &relations)
}
pub fn materialize(
    mut memories: Vec<Memory>,
    relations: &[crate::store::Relation],
) -> Result<Files> {
    let at = crate::store::now();
    memories.retain(|m| !m.deleted && m.expires_at.is_none_or(|expiry| expiry > at));
    let active: BTreeMap<String, i64> = memories
        .iter()
        .map(|m| (m.id.clone(), m.revision))
        .collect();
    memories.sort_by(|a, b| a.id.cmp(&b.id));
    let mut files = BTreeMap::new();
    let mut root = "# Memory\n\n".to_string();
    for memory in memories.iter().filter(|m| m.kind == "preference").take(8) {
        root.push_str(&format!(
            "- {} [[notes/{}]] [source: records/{}.json]\n",
            markdown(&memory.title),
            memory.id,
            memory.id
        ));
    }
    root.push_str("\n## Index\n");
    for (page, group) in memories.chunks(32).enumerate() {
        let path = format!("index/page-{page:04}");
        root.push_str(&format!("- [[{path}]]\n"));
        let mut index = "# Memory records\n\n".to_string();
        for memory in group {
            let id = &memory.id;
            ensure!(
                uuid::Uuid::parse_str(id).is_ok(),
                "The memory ID is invalid."
            );
            index.push_str(&format!("- [[notes/{id}]] {}\n", markdown(&memory.title)));
            let mut note = format!(
                "# Memory record\n\n- {} [[content/{id}.txt]] [source: records/{id}.json; revision: {}]\n- [[records/{id}.json]]\n",
                markdown(&memory.title),
                memory.revision
            );
            for edge in relations {
                if edge.from == *id
                    && edge.from_revision == memory.revision
                    && active.get(&edge.to) == Some(&edge.to_revision)
                {
                    note.push_str(&format!("- [[notes/{}]]\n", edge.to));
                }
            }
            files.insert(format!("notes/{id}.md"), note);
            files.insert(format!("content/{id}.txt"), memory.content.clone());
            let mut record = serde_json::to_value(memory)?;
            record.as_object_mut().unwrap().remove("content");
            record["profile"] = PROFILE.into();
            files.insert(
                format!("records/{id}.json"),
                serde_json::to_string_pretty(&record)? + "\n",
            );
        }
        files.insert(format!("{path}.md"), index);
    }
    files.insert("MEMORY.md".into(), root);
    let result = Files { files };
    ensure!(
        validate_inner(&result, false).accepted,
        "The memory repository is above its limits or has invalid files."
    );
    Ok(result)
}

/// Import only the explicit Enfour profile, never infer facts from arbitrary files.
pub fn import_request(
    store: &Store,
    scope: &str,
    mut metadata: serde_json::Value,
    content: String,
    expected_revision: i64,
) -> Result<crate::store::Remember> {
    ensure!(
        metadata["profile"] == PROFILE,
        "The record profile is not supported."
    );
    ensure!(
        metadata.is_object(),
        "The record metadata must be an object."
    );
    metadata["content"] = content.into();
    let memory: Memory = serde_json::from_value(metadata)?;
    ensure!(
        memory.scope == scope && !memory.deleted,
        "The record must be active and in the selected scope."
    );
    if expected_revision > 0 {
        let current = store.get(scope, &memory.key)?;
        ensure!(
            current.id == memory.id,
            "The record ID changed. Inspect the current record first."
        );
        ensure!(
            memory.revision == expected_revision,
            "The exported revision is different from `expected_revision`."
        );
    }
    Ok(crate::store::Remember {
        scope: scope.into(),
        key: memory.key,
        title: memory.title,
        content: memory.content,
        kind: memory.kind,
        source: memory.source,
        expected_revision,
        expires_at: memory.expires_at,
    })
}
