//! Mechanical checks aid review. They do not prove complete STE compliance.
pub mod dictionary;
pub mod migration;
pub mod text;
use crate::{cache::ExactCache, store::Remember};
use anyhow::Result;
use harper_core::{
    Dialect, Document,
    linting::{LintGroup, Linter},
    parsers::PlainEnglish,
    spell::FstDictionary,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ops::Range, path::Path, sync::Arc};
pub const POLICY: &str = "enfour-ste9-20-v1";
pub const VALIDATOR: &str = "enfour-language-v1";
pub const GLOSSARY: &str = include_str!("../../assets/technical-glossary.tsv");
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Advisory,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Diagnostic {
    pub rule: String,
    pub field: String,
    pub range: Range<usize>,
    pub severity: Severity,
    pub guidance: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Stamp {
    pub policy: String,
    pub validator: String,
    pub dictionary: String,
    pub glossary: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub accepted: bool,
    pub versions: Stamp,
    pub diagnostics: Vec<Diagnostic>,
    pub review: String,
}
#[derive(Debug)]
pub struct Rejected(pub Report);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The memory failed the writing checks. Repair the errors and try again.")
    }
}
impl std::error::Error for Rejected {}
#[derive(Hash, Eq, PartialEq)]
struct Key {
    field: String,
    text: String,
    fingerprint: String,
}
pub struct Language {
    words: Option<BTreeMap<String, Vec<String>>>,
    unavailable: Option<String>,
    stamp: Stamp,
    cache: ExactCache<Key, Arc<Vec<Diagnostic>>>,
    harper: Option<LintGroup>,
}
impl Language {
    pub fn load(path: &Path) -> Self {
        match dictionary::Dictionary::load(path) {
            Ok(d) => Self::new(Some(d.approved()), d.entries_sha256, None),
            Err(e) => Self::new(None, "unavailable".into(), Some(format!("{e:#}"))),
        }
    }
    fn new(
        mut words: Option<BTreeMap<String, Vec<String>>>,
        dictionary: String,
        unavailable: Option<String>,
    ) -> Self {
        if let Some(words) = &mut words {
            for line in GLOSSARY
                .lines()
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
            {
                let (word, pos) = line.split_once('\t').expect("valid bundled glossary");
                words
                    .entry(word.into())
                    .or_default()
                    .extend(pos.split(',').map(str::to_string));
            }
        }
        Self {
            words,
            unavailable,
            stamp: Stamp {
                policy: POLICY.into(),
                validator: VALIDATOR.into(),
                dictionary,
                glossary: dictionary::digest(GLOSSARY),
            },
            cache: ExactCache::new(|k: &Key, v: &Arc<Vec<Diagnostic>>| {
                k.field.len()
                    + k.text.len()
                    + k.fingerprint.len()
                    + v.iter()
                        .map(|d| {
                            d.field.len()
                                + d.rule.len()
                                + d.guidance.len()
                                + size_of::<Diagnostic>()
                        })
                        .sum::<usize>()
            }),
            harper: None,
        }
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_fixture() -> Self {
        let words = include_str!("../../tests/fixtures/language-words.txt")
            .split_whitespace()
            .map(|w| (w.into(), vec!["n".into(), "v".into(), "adj".into()]))
            .collect();
        Self::new(Some(words), "original-test-fixture".into(), None)
    }
    pub fn set_cache_enabled(&mut self, enabled: bool) {
        self.cache.set_enabled(enabled);
    }
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({"available":self.words.is_some(), "versions":self.stamp, "cache":self.cache.info()})
    }
    pub fn validate(&mut self, r: &Remember) -> Report {
        if let Err(error) = crate::store::Store::validate(r) {
            return self.report(vec![diag(
                "enfour.schema",
                "request",
                0..0,
                &error.to_string(),
            )]);
        }
        let mut diagnostics = self.check("title", &r.title);
        diagnostics.extend(self.check("content", &r.content));
        if r.source.trim().is_empty() {
            diagnostics.push(diag(
                "enfour.source",
                "source",
                0..r.source.len(),
                "Add a source reference for the evidence.",
            ));
        }
        if !text::has_explanation(&r.content) {
            diagnostics.push(diag(
                "enfour.explanation",
                "content",
                0..r.content.len(),
                "Add prose about the evidence. Keep the exact evidence in code or a quotation.",
            ));
        }
        self.report(diagnostics)
    }
    pub fn document(&mut self, field: &str, source: &str) -> Report {
        let diagnostics = self.check(field, source);
        self.report(diagnostics)
    }
    fn report(&self, mut diagnostics: Vec<Diagnostic>) -> Report {
        diagnostics.sort_by(|a, b| {
            (&a.field, a.range.start, a.range.end, &a.rule, &a.guidance).cmp(&(
                &b.field,
                b.range.start,
                b.range.end,
                &b.rule,
                &b.guidance,
            ))
        });
        Report { accepted: !diagnostics.iter().any(|d| matches!(d.severity,Severity::Error)), versions: self.stamp.clone(), diagnostics,
            review: "Review the meaning, negation, quantities, conditions, and uncertainty. These checks do not show complete STE compliance.".into() }
    }
    pub fn require_available(&mut self) -> Result<()> {
        if self.words.is_none() {
            let findings = self.check("request", "");
            return Err(Rejected(self.report(findings)).into());
        }
        Ok(())
    }
    pub fn require(&mut self, r: &Remember) -> Result<Report> {
        let report = self.validate(r);
        if !report.accepted {
            return Err(Rejected(report).into());
        }
        Ok(report)
    }
    fn check(&mut self, field: &str, source: &str) -> Vec<Diagnostic> {
        let key = Key {
            field: field.into(),
            text: source.into(),
            fingerprint: format!(
                "{}:{}:{}:{}",
                POLICY, VALIDATOR, self.stamp.dictionary, self.stamp.glossary
            ),
        };
        if let Some(hit) = self.cache.get(&key) {
            return (*hit).clone();
        }
        let Some(words) = &self.words else {
            return vec![diag(
                "enfour.language_data",
                field,
                0..0,
                &format!(
                    "Writes are blocked. Install the verified private language data.\n{}",
                    self.unavailable
                        .as_deref()
                        .unwrap_or("Language data is missing.")
                ),
            )];
        };
        let view = text::View::new(source);
        let mut findings = Vec::new();
        for range in &view.blocks {
            let sentences = text::sentences(&view.prose[range.clone()]);
            if sentences.len() > 6 {
                findings.push(diag(
                    "6.6",
                    field,
                    range.clone(),
                    "Use at most six sentences in one paragraph.",
                ));
            }
            for sentence in sentences {
                let span = range.start + sentence.start..range.start + sentence.end;
                if text::count_words(&view.prose[span.clone()]) > 20 {
                    findings.push(diag(
                        "enfour.20",
                        field,
                        span,
                        "Use at most 20 words in one sentence. Preserve all facts.",
                    ));
                }
            }
        }
        for inner in text::parentheticals(&view.prose) {
            for sentence in text::sentences(&view.prose[inner.clone()]) {
                let span = inner.start + sentence.start..inner.start + sentence.end;
                if text::count_words(&view.prose[span.clone()]) > 20 {
                    findings.push(diag(
                        "8.5",
                        field,
                        span,
                        "Use at most 20 words in each parenthetical sentence.",
                    ));
                }
            }
        }
        for (i, c) in view.prose.char_indices() {
            if c == ';' {
                findings.push(diag("8.1",field,i..i+1,"Replace the prose semicolon with a sentence boundary or a correct conjunction."));
            }
        }
        let tokens = text::words(&view.prose);
        for (index, range) in tokens.iter().enumerate() {
            let token = &view.prose[range.clone()];
            let word = token.to_lowercase().replace('’', "'");
            if word.ends_with("n't")
                || word.ends_with("'re")
                || word.ends_with("'ve")
                || word.ends_with("'ll")
                || word == "i'm"
                || word == "let's"
            {
                findings.push(diag(
                    "4.2",
                    field,
                    range.clone(),
                    "Write the full words. Preserve the negation.",
                ));
                continue;
            }
            if token == "Z" || text::identifier(token) || text::number_or_unit(token) {
                continue;
            }
            if word.ends_with("'s") || word.ends_with("'d") {
                findings.push(advisory(
                    "4.2",
                    field,
                    range.clone(),
                    "Review this apostrophe. Expand a contraction, but keep a possessive.",
                ));
                continue;
            }
            let approved = (word.contains(['-', '‐', '‑'])
                && word
                    .split(['-', '‐', '‑'])
                    .all(|part| words.contains_key(part) || text::number_or_unit(part)))
                || words.contains_key(&word)
                || noun_form(words, &word)
                || phrase_member(words, &tokens, index, &view.prose);
            let uncertain_form = possible_verb_form(words, &word);
            if uncertain_form && !approved {
                findings.push(advisory(
                    "3.1",
                    field,
                    range.clone(),
                    "Review this verb form against the dictionary. Do not change the meaning.",
                ));
            }
            if !approved && !uncertain_form {
                if token.chars().next().is_some_and(char::is_uppercase) && index > 0 {
                    findings.push(advisory("1.5",field,range.clone(),"Review this proper name or technical noun. Add a repeated term to the technical glossary."));
                } else {
                    findings.push(diag("1.1",field,range.clone(),"Use an approved word, or add a reviewed technical term with its permitted part of speech."));
                }
            }
            if word.ends_with("ing") {
                findings.push(advisory(
                    "3.5",
                    field,
                    range.clone(),
                    "Use this -ing form only as a technical noun or a modifier.",
                ));
            }
        }
        // Harper offsets count Unicode scalar values, not UTF-8 bytes.
        let linter = self.harper.get_or_insert_with(|| {
            LintGroup::new_curated(FstDictionary::curated(), Dialect::American)
        });
        for range in &view.blocks {
            let prose = &view.prose[range.clone()];
            let document = Document::new_curated(prose, &PlainEnglish);
            let offsets: Vec<_> = prose
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(prose.len()))
                .collect();
            for token in document.tokens() {
                if let (Some(start), Some(end)) =
                    (offsets.get(token.span.start), offsets.get(token.span.end))
                {
                    let word = prose[*start..*end].to_lowercase();
                    if let Some(allowed) = words.get(&word) {
                        let kind = &token.kind;
                        let candidates = [
                            ("n", kind.is_noun()),
                            ("v", kind.is_verb()),
                            ("adj", kind.is_adjective()),
                            ("adv", kind.is_adverb()),
                        ];
                        if candidates.iter().any(|(_, yes)| *yes)
                            && !candidates
                                .iter()
                                .any(|(pos, yes)| *yes && allowed.iter().any(|p| p == pos))
                        {
                            findings.push(advisory("1.2",field,range.start+start..range.start+end,
                                "Review the part of speech. Use this term only in its approved grammatical role."));
                        }
                    }
                }
            }
            for lint in linter.lint(&document) {
                if let (Some(start), Some(end)) =
                    (offsets.get(lint.span.start), offsets.get(lint.span.end))
                {
                    if view
                        .protected
                        .iter()
                        .any(|p| p.start < range.start + end && p.end > range.start + start)
                    {
                        continue;
                    }
                    findings.push(advisory(
                        "harper.grammar",
                        field,
                        range.start + start..range.start + end,
                        &lint.message,
                    ));
                }
            }
        }
        for pair in tokens.windows(2) {
            let a = view.prose[pair[0].clone()].to_lowercase();
            let b = view.prose[pair[1].clone()].to_lowercase();
            if ["is", "are", "was", "were", "be", "been"].contains(&a.as_str())
                && (b.ends_with("ed")
                    || ["written", "given", "known", "shown"].contains(&b.as_str()))
            {
                findings.push(advisory("3.6",field,pair[0].start..pair[1].end,"Review this possible passive construction. Use an active subject when the actor is known."));
            }
        }
        for group in tokens.windows(4) {
            if group.iter().all(|r| {
                words
                    .get(&view.prose[r.clone()].to_lowercase())
                    .is_some_and(|p| p.iter().any(|p| p == "n"))
            }) {
                findings.push(advisory("2.1",field,group[0].start..group[3].end,"Review this possible noun group. Define a shorter technical name if necessary."));
            }
        }
        self.cache.insert(key, Arc::new(findings.clone()));
        findings
    }
}
fn possible_verb_form(words: &BTreeMap<String, Vec<String>>, word: &str) -> bool {
    for suffix in ["ing", "ed", "es", "s"] {
        if let Some(stem) = word.strip_suffix(suffix) {
            let mut candidates = vec![stem.to_string(), format!("{stem}e")];
            if let Some(stem) = stem.strip_suffix('i') {
                candidates.push(format!("{stem}y"));
            }
            let chars: Vec<_> = stem.char_indices().collect();
            if chars.len() > 1 && chars[chars.len() - 1].1 == chars[chars.len() - 2].1 {
                candidates.push(stem[..chars[chars.len() - 1].0].to_string());
            }
            if candidates
                .iter()
                .any(|s| words.get(s).is_some_and(|p| p.iter().any(|p| p == "v")))
            {
                return true;
            }
        }
    }
    false
}
fn noun_form(words: &BTreeMap<String, Vec<String>>, w: &str) -> bool {
    let candidates = [
        w.strip_suffix('s').map(str::to_string),
        w.strip_suffix("es").map(str::to_string),
        w.strip_suffix("ies").map(|s| format!("{s}y")),
    ];
    candidates
        .iter()
        .flatten()
        .any(|s| words.get(s).is_some_and(|p| p.iter().any(|p| p == "n")))
}
fn phrase_member(
    words: &BTreeMap<String, Vec<String>>,
    tokens: &[Range<usize>],
    at: usize,
    text: &str,
) -> bool {
    for start in at.saturating_sub(3)..=at {
        for end in at + 1..=(start + 4).min(tokens.len()) {
            let phrase = tokens[start..end]
                .iter()
                .map(|r| text[r.clone()].to_lowercase())
                .collect::<Vec<_>>()
                .join(" ");
            if words.contains_key(&phrase) {
                return true;
            }
        }
    }
    false
}
pub fn diag(rule: &str, field: &str, range: Range<usize>, guidance: &str) -> Diagnostic {
    Diagnostic {
        rule: rule.into(),
        field: field.into(),
        range,
        severity: Severity::Error,
        guidance: guidance.into(),
    }
}
fn advisory(rule: &str, field: &str, range: Range<usize>, guidance: &str) -> Diagnostic {
    let mut d = diag(rule, field, range, guidance);
    d.severity = Severity::Advisory;
    d
}
