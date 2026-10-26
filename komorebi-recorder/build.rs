//! Index every log statement in komorebi's source.
//!
//! komorebi's log says what happened but not where in its code, and a recording is only useful for
//! finding a fix if a line of the log can be traced to the statement that wrote it. This scans the
//! `tracing` macros (`info!`, `warn!`, ...) and `#[instrument]` functions of the komorebi crate and
//! writes a catalog (`log_catalog.rs` in `OUT_DIR`) of each statement's level, module, message
//! template, file, line and enclosing function, together with the commit the source is at, so that
//! `komorebi-recorder code` can answer "which line wrote this?" and say when the answer may be
//! about a different version of komorebi than the one that was recorded.
//!
//! If the source is not there (the recorder built on its own) the catalog is empty and says so.

use proc_macro2::TokenStream;
use proc_macro2::TokenTree;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use syn::visit::Visit;

/// A source tree to index: its directory under the repository, and its Rust crate name.
const CRATES: &[(&str, &str)] = &[
    ("komorebi/src", "komorebi"),
    ("komorebi-client/src", "komorebi_client"),
];

struct Entry {
    level: String,
    target: String,
    template: String,
    file: String,
    line: usize,
    function: String,
    owner: String,
}

struct SpanEntry {
    name: String,
    file: String,
    line: usize,
    function: String,
    owner: String,
}

struct Finder<'a> {
    file: String,
    modules: Vec<String>,
    functions: Vec<String>,
    owners: Vec<String>,
    logs: &'a mut Vec<Entry>,
    spans: &'a mut Vec<SpanEntry>,
}

fn level_of(name: &str) -> Option<&'static str> {
    match name {
        "error" => Some("ERROR"),
        "warn" => Some("WARN"),
        "info" => Some("INFO"),
        "debug" => Some("DEBUG"),
        "trace" => Some("TRACE"),
        _ => None,
    }
}

/// The comma-separated top-level arguments of a macro call.
fn arguments(tokens: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut all = vec![Vec::new()];
    for token in tokens {
        match &token {
            TokenTree::Punct(punct) if punct.as_char() == ',' => all.push(Vec::new()),
            _ => all.last_mut().expect("never empty").push(token),
        }
    }
    all
}

fn string_literal(tokens: &[TokenTree]) -> Option<String> {
    let [TokenTree::Literal(literal)] = tokens else {
        return None;
    };
    let stream: TokenStream = TokenTree::Literal(literal.clone()).into();
    syn::parse2::<syn::LitStr>(stream)
        .ok()
        .map(|literal| literal.value())
}

impl Finder<'_> {
    fn target(&self) -> String {
        self.modules.join("::")
    }

    fn function(&self) -> String {
        self.functions.last().cloned().unwrap_or_default()
    }

    fn owner(&self) -> String {
        self.owners.last().cloned().unwrap_or_default()
    }

    fn instrumented(&mut self, attributes: &[syn::Attribute], function: &str) {
        for attribute in attributes {
            let path = attribute.path();
            let last = path
                .segments
                .last()
                .map(|segment| segment.ident.to_string());
            if last.as_deref() != Some("instrument") {
                continue;
            }

            // #[instrument(name = "...")] renames the span
            let mut name = function.to_string();
            let _ = attribute.parse_nested_meta(|meta| {
                if meta.path.is_ident("name") {
                    if let Ok(value) = meta.value().and_then(|value| value.parse::<syn::LitStr>()) {
                        name = value.value();
                    }
                } else if meta.input.peek(syn::Token![=]) {
                    let _ = meta.value().and_then(|value| value.parse::<syn::Expr>());
                } else if meta.input.peek(syn::token::Paren) {
                    let _ = meta.input.parse::<TokenTree>();
                }
                Ok(())
            });

            self.spans.push(SpanEntry {
                name,
                file: self.file.clone(),
                line: path
                    .segments
                    .first()
                    .map_or(0, |segment| segment.ident.span().start().line),
                function: function.to_string(),
                owner: self.owner(),
            });
        }
    }
}

impl<'ast> Visit<'ast> for Finder<'_> {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        self.modules.push(module.ident.to_string());
        syn::visit::visit_item_mod(self, module);
        self.modules.pop();
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let owner = match &*item.self_ty {
            syn::Type::Path(path) => path
                .path
                .segments
                .last()
                .map(|segment| segment.ident.to_string())
                .unwrap_or_default(),
            _ => String::new(),
        };
        self.owners.push(owner);
        syn::visit::visit_item_impl(self, item);
        self.owners.pop();
    }

    fn visit_item_fn(&mut self, function: &'ast syn::ItemFn) {
        let name = function.sig.ident.to_string();
        self.instrumented(&function.attrs, &name);
        self.functions.push(name);
        syn::visit::visit_item_fn(self, function);
        self.functions.pop();
    }

    fn visit_impl_item_fn(&mut self, function: &'ast syn::ImplItemFn) {
        let name = function.sig.ident.to_string();
        self.instrumented(&function.attrs, &name);
        self.functions.push(name);
        syn::visit::visit_impl_item_fn(self, function);
        self.functions.pop();
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let last = mac
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        let Some(level) = last.as_deref().and_then(level_of) else {
            // another macro may wrap code that logs (objc2's `define_class!` holds whole impls):
            // read what it wraps as items, then as statements, and look inside
            if let Ok(file) = syn::parse2::<syn::File>(mac.tokens.clone()) {
                self.visit_file(&file);
            } else if let Ok(statements) =
                syn::parse::Parser::parse2(syn::Block::parse_within, mac.tokens.clone())
            {
                for statement in &statements {
                    self.visit_stmt(statement);
                }
            }
            return;
        };

        // only tracing's: `tracing::info!` or a bare `info!`
        let qualified = mac.path.segments.len() == 1
            || mac
                .path
                .segments
                .first()
                .is_some_and(|segment| segment.ident == "tracing");
        if !qualified {
            return;
        }

        let mut target = self.target();
        let mut template = None;
        for argument in arguments(mac.tokens.clone()) {
            // `target: "x"` overrides the module as the log target
            if let [TokenTree::Ident(name), TokenTree::Punct(colon), rest @ ..] =
                argument.as_slice()
                && name == "target"
                && colon.as_char() == ':'
            {
                if let Some(text) = string_literal(rest) {
                    target = text;
                }
                continue;
            }

            if template.is_none() {
                template = string_literal(&argument);
            }
        }

        self.logs.push(Entry {
            level: level.to_string(),
            target,
            template: template.unwrap_or_default(),
            file: self.file.clone(),
            line: mac
                .path
                .segments
                .first()
                .map_or(0, |segment| segment.ident.span().start().line),
            function: self.function(),
            owner: self.owner(),
        });
    }
}

fn source_files(directory: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            source_files(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            found.push(path);
        }
    }
}

/// `komorebi/src/a/b.rs` under crate `komorebi` is module `komorebi::a::b`.
fn module_of(crate_name: &str, root: &Path, file: &Path) -> Vec<String> {
    let relative = file.strip_prefix(root).unwrap_or(file);
    let mut modules = vec![crate_name.to_string()];
    let parts: Vec<String> = relative
        .with_extension("")
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        if last && (part == "mod" || part == "lib" || part == "main") {
            continue;
        }
        modules.push(part.clone());
    }
    modules
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let repo = manifest_dir
        .parent()
        .expect("a workspace member")
        .to_path_buf();

    println!("cargo:rerun-if-changed=build.rs");

    let mut logs = Vec::new();
    let mut spans = Vec::new();
    let mut files_scanned = 0_usize;
    let mut parse_failures = Vec::new();

    for (directory, crate_name) in CRATES {
        let root = repo.join(directory);
        println!("cargo:rerun-if-changed={}", root.display());

        let mut files = Vec::new();
        source_files(&root, &mut files);
        for path in files {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };

            let relative = path
                .strip_prefix(&repo)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            match syn::parse_file(&text) {
                Ok(file) => {
                    files_scanned += 1;
                    let mut finder = Finder {
                        file: relative,
                        modules: module_of(crate_name, &root, &path),
                        functions: Vec::new(),
                        owners: Vec::new(),
                        logs: &mut logs,
                        spans: &mut spans,
                    };
                    finder.visit_file(&file);
                }
                Err(error) => parse_failures.push(format!("{relative}: {error}")),
            }
        }
    }

    for failure in &parse_failures {
        println!("cargo:warning=komorebi-recorder could not index {failure}");
    }
    if files_scanned == 0 {
        println!(
            "cargo:warning=komorebi's source was not found next to the recorder: `code` will not be \
             able to map log lines to source"
        );
    }

    // the commit the indexed source is at, and whether it has uncommitted changes
    if repo.join(".git").exists() {
        println!(
            "cargo:rerun-if-changed={}",
            repo.join(".git/HEAD").display()
        );
    }
    let commit = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_default();
    let dirty = !commit.is_empty()
        && git(
            &repo,
            &[
                "status",
                "--porcelain",
                "--",
                "komorebi/src",
                "komorebi-client/src",
            ],
        )
        .is_some_and(|status| !status.is_empty());

    let mut out = String::new();
    let _ = writeln!(out, "pub const COMMIT: &str = {commit:?};");
    let _ = writeln!(out, "pub const DIRTY: bool = {dirty};");
    let _ = writeln!(out, "pub const FILES_SCANNED: usize = {files_scanned};");
    let _ = writeln!(out, "pub static ENTRIES: &[RawEntry] = &[");
    for entry in &logs {
        let _ = writeln!(
            out,
            "    RawEntry {{ level: {:?}, target: {:?}, template: {:?}, file: {:?}, line: {}, function: {:?}, owner: {:?} }},",
            entry.level,
            entry.target,
            entry.template,
            entry.file,
            entry.line,
            entry.function,
            entry.owner
        );
    }
    let _ = writeln!(out, "];");
    let _ = writeln!(out, "pub static SPANS: &[RawSpan] = &[");
    for span in &spans {
        let _ = writeln!(
            out,
            "    RawSpan {{ name: {:?}, file: {:?}, line: {}, function: {:?}, owner: {:?} }},",
            span.name, span.file, span.line, span.function, span.owner
        );
    }
    let _ = writeln!(out, "];");

    fs::write(out_dir.join("log_catalog.rs"), out).expect("write log_catalog.rs");
}
