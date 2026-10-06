//! Private Issue 9 data. The public repository contains no extracted entries.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};
pub const PDF_SHA256: &str = "d1f4ea9e7cd6e46b47aa9057209f99e78c0e9cfc4e27a5b07895b05c1a166431";
pub const ENTRIES_SHA256: &str = "b6fff20c798e1f0fa43b0a50bc0bce301b82d8f388ef47ffdc5135d5fb313bff";
pub const IMPORTER: &str = "issue9-bbox-v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub word: String,
    pub pos: String,
    pub approved: bool,
    pub forms: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dictionary {
    pub pdf_sha256: String,
    pub importer: String,
    pub entries_sha256: String,
    pub entries: Vec<Entry>,
}
pub fn digest(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl Dictionary {
    pub fn load(path: &Path) -> Result<Self> {
        let d: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        d.verify()?;
        Ok(d)
    }
    pub fn verify(&self) -> Result<()> {
        ensure!(
            self.pdf_sha256 == PDF_SHA256 && self.importer == IMPORTER,
            "Language data has an unknown source or parser version."
        );
        ensure!(
            digest(serde_json::to_vec(&self.entries)?) == self.entries_sha256,
            "Language data failed its integrity check."
        );
        ensure!(
            self.entries.len() == 2196 && self.entries_sha256 == ENTRIES_SHA256,
            "Language data has an invalid entry count."
        );
        for e in &self.entries {
            ensure!(
                !e.word.is_empty()
                    && ["n", "v", "adj", "adv", "prep", "conj", "pron", "art"]
                        .contains(&e.pos.as_str()),
                "Language data contains an unknown entry."
            );
        }
        Ok(())
    }
    pub fn approved(&self) -> BTreeMap<String, Vec<String>> {
        let mut words: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for e in &self.entries {
            if e.approved {
                for word in std::iter::once(&e.word).chain(&e.forms) {
                    words
                        .entry(word.to_lowercase())
                        .or_default()
                        .push(e.pos.clone());
                }
            }
        }
        words
    }
}
// Poppler emits XML with positioned words. Read only the first dictionary column,
// not alternative words or examples. Explicit page and geometry checks fail closed.
pub fn import(pdf: &Path, poppler: &Path, output: &Path) -> Result<Dictionary> {
    ensure!(
        !output.exists(),
        "The output path is in use. Select a new path."
    );
    ensure!(
        digest(std::fs::read(pdf)?) == PDF_SHA256,
        "The PDF hash is incorrect for Issue 9."
    );
    let result = std::process::Command::new(poppler)
        .args(["-bbox-layout"])
        .arg(pdf)
        .arg("-")
        .output()?;
    ensure!(
        result.status.success(),
        "The PDF parser failed.\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let lines = columns(&result.stdout)?;
    let entries = parse_entries(&lines)?;
    let d = Dictionary {
        pdf_sha256: PDF_SHA256.into(),
        importer: IMPORTER.into(),
        entries_sha256: digest(serde_json::to_vec(&entries)?),
        entries,
    };
    d.verify()?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    file.write_all(&serde_json::to_vec_pretty(&d)?)?;
    file.sync_all()?;
    Ok(d)
}
fn columns(xml: &[u8]) -> Result<Vec<String>> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_reader(xml);
    let mut page = 0usize;
    let mut words = Vec::new();
    let mut coord = None;
    let mut text = String::new();
    let mut lines = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.name().as_ref() == b"page" => {
                page += 1;
                words.clear();
            }
            Event::Start(e) if e.name().as_ref() == b"word" => {
                let mut x = None;
                let mut y = None;
                for a in e.attributes() {
                    let a = a?;
                    match a.key.as_ref() {
                        b"xMin" => x = Some(std::str::from_utf8(&a.value)?.parse::<f64>()?),
                        b"yMin" => y = Some(std::str::from_utf8(&a.value)?.parse::<f64>()?),
                        _ => {}
                    }
                }
                coord = Some((
                    x.context("Missing word coordinate.")?,
                    y.context("Missing word coordinate.")?,
                ));
                text.clear();
            }
            Event::Text(e) if coord.is_some() => {
                text.push_str(&quick_xml::escape::unescape(&e.decode()?)?)
            }
            Event::End(e) if e.name().as_ref() == b"word" => {
                let (x, y) = coord.take().context("Invalid word boundary.")?;
                words.push((x, y, text.clone()));
            }
            Event::End(e) if e.name().as_ref() == b"page" && (149..=434).contains(&page) => {
                let col = words
                    .iter()
                    .find(|w| w.2 == "ALTERNATIVES")
                    .context("Missing dictionary header.")?
                    .0;
                ensure!(
                    (col - 180.02).abs() < 0.1 || (col - 158.42).abs() < 0.1,
                    "Unknown dictionary column."
                );
                let mut first: Vec<_> = words
                    .iter()
                    .filter(|w| w.0 < col - 6.0 && w.1 > 90.0 && w.1 < 718.0)
                    .collect();
                first.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
                let mut y = -100.;
                let mut line = String::new();
                for w in first {
                    if (w.1 - y).abs() > 2.0 && !line.is_empty() {
                        lines.push(std::mem::take(&mut line));
                    }
                    if !line.is_empty() {
                        line.push(' ');
                    }
                    line.push_str(&w.2);
                    y = w.1;
                }
                if !line.is_empty() {
                    lines.push(line);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    ensure!(page == 434, "The PDF has an unknown page count.");
    Ok(lines)
}
fn parse_entries(lines: &[String]) -> Result<Vec<Entry>> {
    let tags = ["n", "v", "adj", "adv", "prep", "conj", "pron", "art"];
    let mut entries: Vec<Entry> = Vec::new();
    let mut pending = String::new();
    for (i, line) in lines.iter().enumerate() {
        // Issue 9 prints this approved phrase without a parenthesized POS.
        if line == "FOR EXAMPLE" {
            ensure!(
                pending.is_empty(),
                "The phrase has an unknown continuation."
            );
            entries.push(Entry {
                word: line.clone(),
                pos: "adv".into(),
                approved: true,
                forms: Vec::new(),
            });
            continue;
        }
        if [
            "No other verb",
            "forms.",
            "No other",
            "forms of this",
            "adjective.",
            "adjective",
        ]
        .contains(&line.as_str())
        {
            continue;
        }
        let pos = tags.iter().find(|tag| line.contains(&format!("({tag})")));
        if let Some(pos) = pos {
            if !pending.is_empty() && !pending.ends_with('-') {
                pending.push(' ');
            }
            if pending.ends_with('-') {
                pending.pop();
            }
            pending.push_str(line);
            let at = pending.rfind(&format!("({pos})")).unwrap();
            let word = pending[..at].trim().to_string();
            ensure!(
                !word.is_empty()
                    && pending[at + pos.len() + 2..]
                        .trim_matches(',')
                        .trim()
                        .is_empty(),
                "Unknown dictionary entry: {pending}"
            );
            let approved = word
                .chars()
                .filter(|c| c.is_alphabetic())
                .all(|c| c.is_uppercase());
            // The one explicit spelling alias is handled without adding the word 'or'.
            let (word, forms) = if word == "MATT (or MATTE)" {
                ("MATT".into(), vec!["MATTE".into()])
            } else {
                (word, Vec::new())
            };
            entries.push(Entry {
                word,
                pos: pos.to_string(),
                approved: approved || pending.starts_with("MATT (or MATTE)"),
                forms,
            });
            pending.clear();
        } else {
            let next_has_tag = lines
                .get(i + 1)
                .is_some_and(|l| tags.iter().any(|tag| l.starts_with(&format!("({tag})"))));
            let is_form = !next_has_tag
                && !line.ends_with('-')
                && entries
                    .last()
                    .is_some_and(|e| e.approved && (e.pos == "v" || e.pos == "adj"))
                && line
                    .chars()
                    .all(|c| c.is_uppercase() || " ,().".contains(c) || c == '-');
            if is_form || line.starts_with("(also ") {
                ensure!(
                    pending.is_empty(),
                    "The dictionary entry is unknown.\n{pending}"
                );
                let e = entries.last_mut().context("Inflection without an entry.")?;
                for form in line
                    .trim_matches(['(', ')'])
                    .trim_start_matches("also ")
                    .split(',')
                {
                    let form = form.trim();
                    if !form.is_empty() {
                        e.forms.push(form.to_string());
                    }
                }
            } else {
                ensure!(
                    pending.is_empty()
                        || pending.ends_with('-')
                        || line.starts_with('(')
                        || line.ends_with("(prep)"),
                    "The dictionary entry is unknown.\n{pending} / {line}"
                );
                if pending.ends_with('-') {
                    pending.pop();
                } else if !pending.is_empty() {
                    pending.push(' ');
                }
                pending.push_str(line);
            }
        }
    }
    ensure!(
        pending.is_empty(),
        "The dictionary entry is not complete.\n{pending}"
    );
    Ok(entries)
}
