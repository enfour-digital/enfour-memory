//! Development-only product prose check. Uses the runtime validator.
use anyhow::Result;
use enfour_memory::language::{Language, Severity};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use syn::{Expr, ExprLit, Lit, Meta, parse::Parser, visit::Visit};
struct Prose {
    strings: BTreeSet<String>,
    docs: bool,
}
impl Prose {
    fn expression(&mut self, e: &Expr) {
        if let Expr::Array(array) = e {
            for element in &array.elems {
                self.expression(element);
            }
        }
        if let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = e
        {
            self.strings.insert(s.value());
        }
    }
}
impl<'ast> Visit<'ast> for Prose {
    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        if self.docs
            && a.path().is_ident("doc")
            && let Meta::NameValue(v) = &a.meta
        {
            self.expression(&v.value);
        }
        // Tool and clap descriptions are nested attribute key-value entries.
        if let Meta::List(list) = &a.meta
            && let Ok(items) = list.parse_args_with(
                syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
            )
        {
            for item in items {
                if let Meta::NameValue(value) = item
                    && (value.path.is_ident("description") || value.path.is_ident("about"))
                {
                    self.expression(&value.value);
                }
            }
        }
        syn::visit::visit_attribute(self, a);
    }
    fn visit_macro(&mut self, e: &'ast syn::Macro) {
        let name = e
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default();
        let parser = syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated;
        if let Ok(args) = parser.parse2(e.tokens.clone()) {
            let i = match name.as_str() {
                "ensure" => Some(1),
                "bail" | "anyhow" | "println" | "eprintln" => Some(0),
                _ => None,
            };
            if let Some(i) = i
                && let Some(arg) = args.get(i)
            {
                self.expression(arg);
            }
        }
        syn::visit::visit_macro(self, e);
    }
    fn visit_expr_call(&mut self, e: &'ast syn::ExprCall) {
        if let Expr::Path(path) = e.func.as_ref() {
            let name = path
                .path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            if ["diag", "advisory"].contains(&name.as_str()) {
                if let Some(last) = e.args.last() {
                    self.expression(last);
                }
            } else if ["invalid_params", "internal_error"].contains(&name.as_str())
                && let Some(first) = e.args.first()
            {
                self.expression(first);
            }
        }
        syn::visit::visit_expr_call(self, e);
    }
    fn visit_expr_method_call(&mut self, e: &'ast syn::ExprMethodCall) {
        if e.method == "add"
            && let Some(last) = e.args.last()
        {
            self.expression(last);
        }
        if (e.method == "context" || e.method == "with_instructions")
            && let Some(first) = e.args.first()
        {
            self.expression(first);
        }
        syn::visit::visit_expr_method_call(self, e);
    }
    fn visit_item_const(&mut self, c: &'ast syn::ItemConst) {
        if c.ident.to_string().contains("INSTRUCTIONS") {
            self.expression(&c.expr);
        }
    }
    fn visit_item_mod(&mut self, m: &'ast syn::ItemMod) {
        // Tests and their counterexamples are not product prose.
        if m.ident != "tests" && !m.attrs.iter().any(|a| a.path().is_ident("cfg")) {
            syn::visit::visit_item_mod(self, m);
        }
    }
}
fn files(path: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let p = entry?.path();
        if p.is_dir() {
            files(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    Ok(())
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().unwrap_or_else(|| ".".into()));
    let dictionary = PathBuf::from(
        args.next()
            .or_else(|| std::env::var("ENFOUR_LANGUAGE").ok())
            .unwrap_or_else(|| "language-private/dictionary.json".into()),
    );
    let mut language = Language::load(&dictionary);
    let mut reports = Vec::new();
    let mut failed = false;
    for name in [
        "README.md",
        "skills/enfour-recall/SKILL.md",
        "skills/enfour-maintain/SKILL.md",
    ] {
        let report = language.document(name, &std::fs::read_to_string(root.join(name))?);
        failed |= !report.accepted;
        reports.push(serde_json::json!({"file":name,"report":report}));
    }
    let html = std::fs::read_to_string(root.join("assets/index.html"))?;
    let body = html
        .split_once("<main>")
        .and_then(|(_, s)| s.split_once("</main>").map(|(s, _)| s))
        .ok_or_else(|| anyhow::anyhow!("Missing dashboard body."))?;
    let mut ui = Vec::new();
    for part in body.split('>') {
        if let Some((text, _)) = part.split_once('<')
            && !text.trim().is_empty()
        {
            ui.push(text.trim().to_string());
        }
    }
    for part in body.split("placeholder=").skip(1) {
        if let Some(text) = part.strip_prefix('"').and_then(|s| s.split('"').next()) {
            ui.push(text.to_string());
        }
    }
    for part in html.split(".textContent=").skip(1) {
        if let Some(quote) = part
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == char::from(39))
            && let Some(text) = part[1..].split(quote).next()
        {
            ui.push(text.replace(&format!("{}n", char::from(92)), " "));
        }
    }
    for text in ui {
        let report = language.document("assets/index.html", &text);
        if !report.accepted {
            failed = true;
            reports
                .push(serde_json::json!({"file":"assets/index.html","text":text,"report":report}));
        }
    }
    let mut paths = Vec::new();
    files(&root.join("src"), &mut paths)?;
    paths.retain(|p| {
        p.file_name()
            .is_some_and(|n| n != "enfour-product-check.rs")
            && !p.to_string_lossy().contains("/bin/enfour-benchmark")
    });
    for path in paths {
        let mut prose = Prose {
            strings: BTreeSet::new(),
            docs: path.file_name().is_some_and(|n| {
                ["main.rs", "server.rs", "store.rs", "skills.rs"]
                    .iter()
                    .any(|x| n == *x)
            }),
        };
        prose.visit_file(&syn::parse_file(&std::fs::read_to_string(&path)?)?);
        for text in prose.strings {
            // Canonical scope output is protocol data, not prose.
            if text.starts_with("repo:") {
                continue;
            }
            // Dynamic values are exact external data. Check only authored prose.
            let mut checked = String::new();
            let mut depth = 0;
            for c in text.chars() {
                if c == '{' {
                    depth += 1;
                    checked.push_str(" value ");
                } else if c == '}' {
                    depth -= 1;
                } else if depth == 0 {
                    checked.push(c);
                }
            }
            if checked.chars().filter(|c| c.is_alphabetic()).count() < 2 {
                continue;
            }
            let report = language.document(&path.display().to_string(), &checked);
            if !report.accepted {
                failed = true;
                reports.push(serde_json::json!({"file":path,"text":text,"report":report}));
            }
        }
    }
    let errors: usize = reports
        .iter()
        .filter_map(|r| r["report"]["diagnostics"].as_array())
        .flatten()
        .filter(|d| d["severity"] == serde_json::to_value(Severity::Error).unwrap())
        .count();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"accepted":!failed,"errors":errors,"reports":reports})
        )?
    );
    if failed {
        std::process::exit(2);
    }
    Ok(())
}
