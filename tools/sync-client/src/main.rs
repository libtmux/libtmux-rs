//! Keep the blocking operation layer in step with the native async client.

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use proc_macro2::Span;
use quote::ToTokens as _;
use sha2::{Digest as _, Sha256};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned as _;
use syn::visit::{self, Visit};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const FILES: &[&str] = &[
    "capabilities.rs",
    "client.rs",
    "command.rs",
    "error.rs",
    "error/classification.rs",
    "error/refusal.rs",
    "error/scoped.rs",
    "hooks.rs",
    "internal/environment.rs",
    "internal/listing.rs",
    "internal/options.rs",
    "layout.rs",
    "options.rs",
    "pane.rs",
    "pane/observe.rs",
    "pane/settings.rs",
    "plan.rs",
    "plan/ops.rs",
    "plan/ops/options.rs",
    "plan/ops/panes.rs",
    "plan/ops/sessions.rs",
    "plan/ops/timing.rs",
    "plan/ops/windows.rs",
    "plan/planner.rs",
    "plan/run.rs",
    "plan/wire.rs",
    "server.rs",
    "server/channels.rs",
    "server/discovery.rs",
    "server/interactive.rs",
    "server/keys.rs",
    "server/settings.rs",
    "session.rs",
    "session/settings.rs",
    "snapshot.rs",
    "window.rs",
    "window/navigation.rs",
    "window/settings.rs",
];

const GENERATED_TESTS: &[&str] = &[
    "command.rs",
    "command_budget.rs",
    "command_chain.rs",
    "examples_manifest.rs",
    "filter_derive.rs",
    "filter_dialect.rs",
    "filter_hierarchy.rs",
    "filter_serde.rs",
    "filter_tree.rs",
    "format_injection.rs",
    "hierarchy.rs",
    "listing_failures.rs",
    "macros_readme.rs",
    "options.rs",
    "plan.rs",
    "plan_schema.rs",
    "positional_args.rs",
    "query.rs",
    "scoped.rs",
    "target.rs",
    "tmux_text.rs",
    "version.rs",
];

const GENERATED_UNIT_TESTS: &[&str] = &[
    "command/tests.rs",
    "error/tests.rs",
    "target/tests.rs",
    "formats/tests.rs",
    "snapshot/tests.rs",
    "error/compat_tests.rs",
];

const GENERATED_EXAMPLES: &[&str] = &[
    "find.rs",
    "inspect.rs",
    "orchestrate.rs",
    "recover.rs",
    "scratch.rs",
    "sweep.rs",
];

const MANUAL_SURFACES: &[&str] = &[
    "tests/blocking.rs",
    "tests/cancel_safety.rs",
    "tests/commands.rs",
    "tests/control.rs",
    "tests/control_correlation.rs",
    "tests/control_mode_routing.rs",
    "tests/instrumentation.rs",
    "tests/limits.rs",
    "tests/mutations.rs",
    "tests/server_command.rs",
    "tests/test_server.rs",
    "examples/api_capture.rs",
    "examples/api_cardinality.rs",
    "examples/api_construction.rs",
    "examples/api_creation.rs",
    "examples/api_input.rs",
    "examples/api_listings.rs",
    "examples/api_query.rs",
    "examples/matrix.rs",
    "examples/watch.rs",
    "benches/hierarchy.rs",
    "benches/waits.rs",
];

fn main() -> Result<()> {
    let mut check = false;
    let mut seed = None;
    let mut only = BTreeSet::new();
    for argument in std::env::args().skip(1) {
        if argument == "--check" {
            check = true;
        } else if let Some(file) = argument.strip_prefix("--seed=") {
            if !MANUAL_SURFACES.contains(&file) {
                return Err(format!("unknown manual surface: {file}").into());
            }
            seed = Some(file.to_owned());
        } else if let Some(file) = argument.strip_prefix("--only=") {
            if !FILES.contains(&file) {
                return Err(format!("unknown generated module: {file}").into());
            }
            only.insert(file.to_owned());
        } else {
            return Err(format!("unknown argument: {argument}").into());
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("the generator must live under tools/sync-client")?;
    if let Some(relative) = seed {
        if check {
            return Err("--seed cannot be combined with --check".into());
        }
        return seed_manual_surface(root, &relative);
    }
    let source_root = root.join("crates/libtmux-tokio/src");
    let target_root = root.join("crates/libtmux/src");
    let ports = fs::read_to_string(root.join("tools/sync-client/test-ports.tsv"))?;
    if check && only.is_empty() {
        verify_source_inventory(root)?;
        verify_doc_ports(root)?;
    }
    let mut stale = Vec::new();
    let mut methods = 0;
    for relative in FILES {
        if !only.is_empty() && !only.contains(*relative) {
            continue;
        }
        let source = fs::read_to_string(source_root.join(relative))?;
        let output = translate(&source, relative, false)?;
        methods += output.methods;
        let generated =
            format_source(&output.source).map_err(|error| format!("{relative}: {error}"))?;
        let target = target_root.join(relative);
        if fs::read_to_string(&target).ok().as_deref() != Some(&generated) {
            if check {
                stale.push((*relative).to_owned());
            } else {
                fs::write(target, generated)?;
            }
        }
        for tests in output.manual_tests {
            verify_test_port(relative, &source, &tests.path, &ports)?;
            let target = target_root.join(&tests.path);
            if !target.exists() && !check {
                let parent = target.parent().ok_or("test module has no parent")?;
                fs::create_dir_all(parent)?;
                let translated = translate(&tests.source, "test module", true)?;
                fs::write(&target, format_source(&translated.source)?)?;
            }
            let existing = fs::read_to_string(&target)?;
            let names = test_names(&syn::parse_file(&existing)?);
            let expected = tests
                .names
                .iter()
                .map(|name| test_port_name(relative, name).to_owned())
                .collect::<BTreeSet<_>>();
            if !expected.is_subset(&names) {
                return Err(format!(
                    "{} is missing native regression tests: {:?}",
                    target.display(),
                    expected.difference(&names).collect::<Vec<_>>()
                )
                .into());
            }
        }
    }
    if only.is_empty() {
        generate_auxiliaries(root, check, &mut stale)?;
    }
    if !stale.is_empty() {
        return Err(format!(
            "blocking sources differ; run the generator: {}",
            stale.join(", ")
        )
        .into());
    }
    println!(
        "{} operation modules; {methods} async function bodies translated",
        FILES.len()
    );
    Ok(())
}

fn seed_manual_surface(root: &Path, relative: &str) -> Result<()> {
    let source = fs::read_to_string(root.join("crates/libtmux-tokio").join(relative))?;
    let output = translate(&source, relative, true)?;
    fs::write(
        root.join("crates/libtmux").join(relative),
        format_source(&output.source)?,
    )?;
    println!(
        "Seeded {relative}; review concurrency and cancellation before recording its canonical hash."
    );
    Ok(())
}

fn generate_auxiliaries(root: &Path, check: bool, stale: &mut Vec<String>) -> Result<()> {
    for (directory, files) in [
        ("src", GENERATED_UNIT_TESTS),
        ("tests", GENERATED_TESTS),
        ("examples", GENERATED_EXAMPLES),
    ] {
        for file in files {
            let relative = format!("{directory}/{file}");
            let source = fs::read_to_string(root.join("crates/libtmux-tokio").join(&relative))?;
            let output = translate(&source, &relative, true)?;
            let generated =
                format_source(&output.source).map_err(|error| format!("{relative}: {error}"))?;
            let target = root.join("crates/libtmux").join(&relative);
            if fs::read_to_string(&target).ok().as_deref() != Some(&generated) {
                if check {
                    stale.push(relative);
                } else {
                    fs::write(target, generated)?;
                }
            }
        }
    }
    Ok(())
}

fn rust_sources(root: &Path) -> Result<BTreeSet<String>> {
    fn visit(directory: &Path, root: &Path, files: &mut BTreeSet<String>) -> Result<()> {
        for entry in fs::read_dir(directory)? {
            let path = entry?.path();
            if path.is_dir() {
                visit(&path, root, files)?;
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.insert(
                    path.strip_prefix(root)?
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
        Ok(())
    }
    let mut files = BTreeSet::new();
    for directory in ["src", "tests", "examples", "benches"] {
        visit(&root.join(directory), root, &mut files)?;
    }
    Ok(files)
}

fn verify_source_inventory(root: &Path) -> Result<()> {
    let native = root.join("crates/libtmux-tokio");
    let blocking = root.join("crates/libtmux");
    let inventory = fs::read_to_string(root.join("tools/sync-client/source-ports.tsv"))?;
    let generated = FILES
        .iter()
        .map(|file| format!("src/{file}"))
        .chain(
            GENERATED_UNIT_TESTS
                .iter()
                .map(|file| format!("src/{file}")),
        )
        .chain(GENERATED_TESTS.iter().map(|file| format!("tests/{file}")))
        .chain(
            GENERATED_EXAMPLES
                .iter()
                .map(|file| format!("examples/{file}")),
        )
        .collect::<BTreeSet<_>>();
    verify_source_inventory_at(&native, &blocking, &inventory, &generated)?;
    verify_blocking_inventory(root, &inventory)?;
    let aliases = fs::read_to_string(root.join("tools/sync-client/test-renames.tsv"))?;
    for record in inventory.lines().filter(|line| !line.starts_with('#')) {
        let columns = record.split('\t').collect::<Vec<_>>();
        if columns.len() != 5 || columns[1] != "manual" || !columns[0].starts_with("tests/") {
            continue;
        }
        let source = columns[0];
        let native_tests = test_names(&syn::parse_file(&fs::read_to_string(native.join(source))?)?);
        let mut blocking_tests = BTreeSet::new();
        for target in columns[2].split(',') {
            blocking_tests.extend(test_names(&syn::parse_file(&fs::read_to_string(
                blocking.join(target),
            )?)?));
        }
        for name in native_tests {
            let target = aliases
                .lines()
                .filter(|line| !line.starts_with('#'))
                .find_map(|line| {
                    let values = line.split('\t').collect::<Vec<_>>();
                    (values.len() == 4
                        && values[0] == source
                        && values[1] == name
                        && !values[3].is_empty())
                    .then(|| values[2])
                })
                .unwrap_or(&name);
            if target != "-" && !blocking_tests.contains(target) {
                return Err(format!(
                    "{source}: native regression test {name} has no blocking counterpart {target}"
                )
                .into());
            }
        }
    }
    Ok(())
}

fn verify_blocking_inventory(root: &Path, native_records: &str) -> Result<()> {
    let mut recorded = BTreeSet::new();
    for record in native_records.lines().filter(|line| !line.starts_with('#')) {
        if let Some(targets) = record.split('\t').nth(2) {
            recorded.extend(
                targets
                    .split(',')
                    .filter(|target| *target != "-")
                    .map(str::to_owned),
            );
        }
    }
    let own = fs::read_to_string(root.join("tools/sync-client/blocking-sources.tsv"))?;
    for line in own
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
    {
        let Some((source, reason)) = line.split_once('\t') else {
            return Err("invalid blocking-sources.tsv row".into());
        };
        if reason.is_empty() || !recorded.insert(source.to_owned()) {
            return Err(format!("duplicate or unexplained blocking source: {source}").into());
        }
    }
    let actual = rust_sources(&root.join("crates/libtmux"))?;
    if recorded != actual {
        return Err(format!(
            "blocking Rust source inventory changed: unrecorded {:?}; removed {:?}",
            actual.difference(&recorded).collect::<Vec<_>>(),
            recorded.difference(&actual).collect::<Vec<_>>()
        )
        .into());
    }
    Ok(())
}

fn verify_doc_ports(root: &Path) -> Result<()> {
    let mut source = String::new();
    for relative in FILES {
        let text = fs::read_to_string(root.join("crates/libtmux-tokio/src").join(relative))?;
        for line in text.lines() {
            source.push_str(doc_line(line).map_or(line, |(_, content)| content));
            source.push(' ');
        }
    }
    let normalized = source.split_whitespace().collect::<Vec<_>>().join(" ");
    for row in include_str!("../doc-ports.tsv").lines() {
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let Some((canonical, blocking)) = row.split_once('\t') else {
            return Err("invalid doc-ports.tsv row".into());
        };
        if blocking.is_empty() || !normalized.contains(canonical) {
            return Err(format!(
                "a manually adapted rustdoc contract changed; review doc-ports.tsv: {canonical}"
            )
            .into());
        }
    }
    Ok(())
}

fn verify_source_inventory_at(
    native: &Path,
    blocking: &Path,
    inventory: &str,
    generated: &BTreeSet<String>,
) -> Result<()> {
    let mut recorded = BTreeSet::new();
    for (line, record) in inventory.lines().enumerate() {
        if record.is_empty() || record.starts_with('#') {
            continue;
        }
        let columns = record.split('\t').collect::<Vec<_>>();
        if columns.len() != 5 || columns[4].is_empty() {
            return Err(format!(
                "source-ports.tsv:{}: expected five nonempty columns",
                line + 1
            )
            .into());
        }
        let source = columns[0];
        let mode = columns[1];
        let targets = columns[2];
        let digest = columns[3];
        if !recorded.insert(source.to_owned()) {
            return Err(format!("duplicate native source in source-ports.tsv: {source}").into());
        }
        let canonical = fs::read(native.join(source))?;
        for target in targets.split(',').filter(|target| *target != "-") {
            let path = Path::new(target);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
                || !blocking.join(path).is_file()
            {
                return Err(format!(
                    "{source}: blocking counterpart is absent or outside the crate: {target}"
                )
                .into());
            }
        }
        match mode {
            "generated" if generated.contains(source) && targets == source && digest == "-" => {}
            "equal" if digest == "-" && canonical == fs::read(blocking.join(targets))? => {}
            "manual" | "native-only" => {
                let actual = format!("{:x}", Sha256::digest(&canonical));
                if digest != actual {
                    return Err(format!("{source}: native source changed; review its blocking counterpart and update source-ports.tsv (sha256 {actual})").into());
                }
                if generated.contains(source) {
                    return Err(format!(
                        "{source}: a generated source must not bypass the generator"
                    )
                    .into());
                }
            }
            _ => {
                return Err(format!("{source}: invalid or stale {mode} source-port record").into());
            }
        }
    }
    let actual = rust_sources(native)?;
    if actual != recorded {
        return Err(format!(
            "native Rust source inventory changed: unrecorded {:?}; removed {:?}",
            actual.difference(&recorded).collect::<Vec<_>>(),
            recorded.difference(&actual).collect::<Vec<_>>()
        )
        .into());
    }
    if !generated.is_subset(&recorded) {
        return Err("a generated source is absent from source-ports.tsv".into());
    }
    Ok(())
}

struct Translation {
    source: String,
    methods: usize,
    manual_tests: Vec<ManualTests>,
}

struct ManualTests {
    path: PathBuf,
    source: String,
    names: BTreeSet<String>,
}

fn translate(source: &str, file: &str, test_module: bool) -> Result<Translation> {
    let source = translate_docs(source).map_err(|error| format!("{file}: rustdoc: {error}"))?;
    let parsed = syn::parse_file(&source).map_err(|error| format!("{file}: {error}"))?;
    let mut visitor = BlockingVisitor::new(&source, file, test_module);
    visitor.visit_file(&parsed);
    if let Some(error) = visitor.error {
        return Err(format!("{file}: {error}").into());
    }
    let methods = visitor.methods;
    let manual_tests = visitor.manual_tests;
    let source = apply_edits(&source, visitor.edits)?;
    syn::parse_file(&source).map_err(|error| format!("translated {file}: {error}"))?;
    Ok(Translation {
        source,
        methods,
        manual_tests,
    })
}

#[derive(Debug)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

struct BlockingVisitor<'a> {
    source: &'a str,
    file: &'a str,
    test_module: bool,
    line_starts: Vec<usize>,
    edits: Vec<Edit>,
    methods: usize,
    manual_tests: Vec<ManualTests>,
    error: Option<String>,
}

impl<'a> BlockingVisitor<'a> {
    fn new(source: &'a str, file: &'a str, test_module: bool) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(index, _)| index + 1));
        Self {
            source,
            file,
            test_module,
            line_starts,
            edits: Vec::new(),
            methods: 0,
            manual_tests: Vec::new(),
            error: None,
        }
    }

    fn offset(&self, position: proc_macro2::LineColumn) -> usize {
        let start = self.line_starts[position.line - 1];
        start
            + self.source[start..]
                .char_indices()
                .nth(position.column)
                .map_or(self.source.len() - start, |(offset, _)| offset)
    }

    fn start(&self, span: Span) -> usize {
        self.offset(span.start())
    }

    fn end(&self, span: Span) -> usize {
        self.offset(span.end())
    }

    fn replace(&mut self, span: Span, replacement: impl Into<String>) {
        self.edits.push(Edit {
            start: self.start(span),
            end: self.end(span),
            replacement: replacement.into(),
        });
    }

    fn range(&mut self, start: usize, end: usize, replacement: impl Into<String>) {
        self.edits.push(Edit {
            start,
            end,
            replacement: replacement.into(),
        });
    }

    fn source_for(&self, span: Span) -> &str {
        &self.source[self.start(span)..self.end(span)]
    }

    fn closure(&mut self, expression: &syn::ExprAsync) {
        let prefix = if expression.capture.is_some() {
            "move || "
        } else {
            "|| "
        };
        self.range(
            self.start(expression.async_token.span),
            self.start(expression.block.brace_token.span.open()),
            prefix,
        );
        self.visit_block(&expression.block);
    }

    fn doc_meta(&mut self, meta: &syn::Meta) {
        match meta {
            syn::Meta::NameValue(value) if value.path.is_ident("doc") => {
                if let syn::Expr::Lit(expression) = &value.value
                    && let syn::Lit::Str(literal) = &expression.lit
                {
                    let markdown = literal.value();
                    let mut as_comments = String::new();
                    for line in markdown.lines() {
                        as_comments.push_str("/// ");
                        as_comments.push_str(line);
                        as_comments.push('\n');
                    }
                    match translate_docs(&as_comments) {
                        Ok(translated) => {
                            let markdown = translated
                                .lines()
                                .filter_map(|line| doc_line(line).map(|(_, content)| content))
                                .collect::<Vec<_>>()
                                .join("\n");
                            let mut hashes = String::from("#");
                            while markdown.contains(&format!("\"{hashes}")) {
                                hashes.push('#');
                            }
                            self.replace(
                                literal.span(),
                                format!("r{hashes}\"{markdown}\"{hashes}"),
                            );
                        }
                        Err(error) => self.error = Some(format!("doc attribute: {error}")),
                    }
                } else if let syn::Expr::Macro(expression) = &value.value
                    && expression.mac.path.is_ident("concat")
                {
                    for token in expression.mac.tokens.clone() {
                        if let proc_macro2::TokenTree::Literal(literal) = token
                            && let Ok(text) = syn::parse_str::<syn::LitStr>(&literal.to_string())
                            && text.value().contains("libtmux_tokio::")
                        {
                            let replacement = syn::LitStr::new(
                                &text.value().replace("libtmux_tokio::", "libtmux::"),
                                literal.span(),
                            );
                            self.replace(literal.span(), replacement.to_token_stream().to_string());
                        }
                    }
                }
            }
            syn::Meta::List(list) if list.path.is_ident("cfg_attr") => {
                match list
                    .parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
                {
                    Ok(items) => {
                        for item in &items {
                            self.doc_meta(item);
                        }
                    }
                    Err(error) => self.error = Some(error.to_string()),
                }
            }
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for BlockingVisitor<'_> {
    fn visit_signature(&mut self, signature: &'ast syn::Signature) {
        if let Some(keyword) = signature.asyncness {
            self.replace(keyword.span, "");
            self.methods += 1;
        }
        visit::visit_signature(self, signature);
    }

    fn visit_expr_await(&mut self, expression: &'ast syn::ExprAwait) {
        self.replace(expression.dot_token.span, "");
        self.replace(expression.await_token.span, "");
        self.visit_expr(&expression.base);
    }

    fn visit_expr_async(&mut self, expression: &'ast syn::ExprAsync) {
        let prefix = if expression.capture.is_some() {
            "(move || "
        } else {
            "(|| "
        };
        self.range(
            self.start(expression.async_token.span),
            self.start(expression.block.brace_token.span.open()),
            prefix,
        );
        self.range(
            self.end(expression.block.brace_token.span.close()),
            self.end(expression.block.brace_token.span.close()),
            ")()",
        );
        self.visit_block(&expression.block);
    }

    fn visit_expr_closure(&mut self, expression: &'ast syn::ExprClosure) {
        if let Some(keyword) = expression.asyncness {
            self.replace(keyword.span, "");
        }
        if let syn::Expr::Async(body) = expression.body.as_ref() {
            self.range(
                self.start(body.async_token.span),
                self.start(body.block.brace_token.span.open()),
                "",
            );
            self.visit_block(&body.block);
            return;
        }
        visit::visit_expr_closure(self, expression);
    }

    fn visit_expr_call(&mut self, expression: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = expression.func.as_ref() {
            let name = path.path.to_token_stream().to_string().replace(' ', "");
            if self.test_module && name == "tokio::spawn" {
                self.replace(expression.func.span(), "std::thread::spawn");
                for argument in &expression.args {
                    match argument {
                        syn::Expr::Async(task) => self.closure(task),
                        syn::Expr::Block(block) => {
                            for statement in &block.block.stmts {
                                if let syn::Stmt::Expr(syn::Expr::Async(task), None) = statement {
                                    self.closure(task);
                                } else {
                                    self.visit_stmt(statement);
                                }
                            }
                        }
                        _ => {
                            self.range(
                                self.start(argument.span()),
                                self.start(argument.span()),
                                "move || ",
                            );
                            self.visit_expr(argument);
                        }
                    }
                }
                return;
            }
            if name.ends_with("scoped::run") {
                self.visit_expr(&expression.func);
                for (index, argument) in expression.args.iter().enumerate() {
                    if index == 0 {
                        if let syn::Expr::MethodCall(call) = argument
                            && call.method == "clone"
                            && call.args.is_empty()
                        {
                            self.replace(
                                argument.span(),
                                self.source_for(call.receiver.span()).to_owned(),
                            );
                        } else {
                            self.range(
                                self.start(argument.span()),
                                self.start(argument.span()),
                                "&",
                            );
                            self.visit_expr(argument);
                        }
                    } else if index == 2
                        && let syn::Expr::Async(create) = argument
                    {
                        self.closure(create);
                    } else {
                        self.visit_expr(argument);
                    }
                }
                return;
            }
            if self.test_module
                && matches!(name.as_str(), "DispatchFuture::new" | "ShutdownFuture::new")
            {
                for argument in &expression.args {
                    if let syn::Expr::Async(task) = argument {
                        self.closure(task);
                    } else {
                        self.visit_expr(argument);
                    }
                }
                return;
            }
        }
        visit::visit_expr_call(self, expression);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let compact = path.to_token_stream().to_string().replace(' ', "");
        if compact.starts_with("tokio::time::Instant") {
            self.replace(
                path.span(),
                compact.replacen("tokio::time::Instant", "std::time::Instant", 1),
            );
            return;
        }
        match compact.as_str() {
            "tokio::time::sleep" => self.replace(path.span(), "std::thread::sleep"),
            "tokio::task::yield_now" => self.replace(path.span(), "std::thread::yield_now"),
            "libtmux::IdParseError" if self.file == "error.rs" => {
                self.replace(path.span(), "crate::primitives::IdParseError");
            }
            _ => visit::visit_path(self, path),
        }
    }

    fn visit_path_segment(&mut self, segment: &'ast syn::PathSegment) {
        match segment.ident.to_string().as_str() {
            "AsyncFnOnce" => self.replace(segment.ident.span(), "FnOnce"),
            "libtmux_tokio" => self.replace(segment.ident.span(), "libtmux"),
            _ => {}
        }
        visit::visit_path_segment(self, segment);
    }

    fn visit_use_tree(&mut self, tree: &'ast syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) if path.ident == "libtmux_tokio" => {
                self.replace(path.ident.span(), "libtmux");
            }
            syn::UseTree::Path(path) if self.file == "error.rs" && path.ident == "libtmux" => {
                self.replace(path.ident.span(), "crate::primitives");
            }
            _ => {}
        }
        visit::visit_use_tree(self, tree);
    }

    fn visit_expr_macro(&mut self, expression: &'ast syn::ExprMacro) {
        if expression
            .mac
            .path
            .to_token_stream()
            .to_string()
            .replace(' ', "")
            == "tokio::try_join"
        {
            let parsed = expression
                .mac
                .parse_body_with(Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated);
            match parsed {
                Ok(arguments) => {
                    let values = arguments
                        .iter()
                        .map(|value| format!("{}?", self.source_for(value.span())))
                        .collect::<Vec<_>>();
                    self.replace(
                        expression.span(),
                        format!("Ok::<_, Error>(({},))", values.join(", ")),
                    );
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        } else {
            visit::visit_expr_macro(self, expression);
        }
    }

    fn visit_macro(&mut self, item: &'ast syn::Macro) {
        fn tokens(visitor: &mut BlockingVisitor<'_>, stream: proc_macro2::TokenStream) {
            let values = stream.into_iter().collect::<Vec<_>>();
            for (index, token) in values.iter().enumerate() {
                match token {
                    proc_macro2::TokenTree::Group(group) => {
                        if group.delimiter() == proc_macro2::Delimiter::Bracket
                            && let Ok(meta) = syn::parse2::<syn::Meta>(group.stream())
                            && meta.path().is_ident("doc")
                        {
                            let original = visitor.source_for(group.span()).trim_start();
                            if !original.starts_with("///") && !original.starts_with("//!") {
                                visitor.doc_meta(&meta);
                            }
                        } else {
                            tokens(visitor, group.stream());
                        }
                    }
                    proc_macro2::TokenTree::Ident(name) if name == "await" && index > 0 => {
                        if let proc_macro2::TokenTree::Punct(dot) = &values[index - 1]
                            && dot.as_char() == '.'
                        {
                            visitor.replace(dot.span(), "");
                            visitor.replace(name.span(), "");
                        }
                    }
                    proc_macro2::TokenTree::Ident(name) if name == "libtmux_tokio" => {
                        visitor.replace(name.span(), "libtmux");
                    }
                    _ => {}
                }
            }
        }
        tokens(self, item.tokens.clone());
    }

    fn visit_attribute(&mut self, attribute: &'ast syn::Attribute) {
        let name = attribute
            .path()
            .to_token_stream()
            .to_string()
            .replace(' ', "");
        if self.test_module && name == "tokio::test" {
            self.replace(attribute.span(), "#[test]");
        } else if self.test_module && name == "tokio::main" {
            self.replace(attribute.span(), "");
        } else if attribute.path().is_ident("derive") {
            match attribute
                .parse_args_with(Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
            {
                Ok(paths) => {
                    for path in &paths {
                        self.visit_path(path);
                    }
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        } else {
            let text = self.source_for(attribute.span()).trim_start();
            if !text.starts_with("///") && !text.starts_with("//!") {
                self.doc_meta(&attribute.meta);
            }
        }
    }

    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        let separate = matches!(
            self.file,
            "server.rs" | "internal/listing.rs" | "plan/run.rs"
        );
        if separate
            && module.ident == "tests"
            && let Some((braces, items)) = &module.content
        {
            let base = self.file.strip_suffix(".rs").unwrap_or(self.file);
            let path = format!("{base}/sync_tests.rs");
            let relative = Path::new(base)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(base);
            self.replace(
                module.span(),
                format!("#[cfg(test)]\n#[path = \"{relative}/sync_tests.rs\"]\nmod tests;"),
            );
            self.manual_tests.push(ManualTests {
                path: PathBuf::from(path),
                source: self.source[self.end(braces.span.open())..self.start(braces.span.close())]
                    .to_owned(),
                names: items.iter().filter_map(test_name).collect(),
            });
        } else {
            visit::visit_item_mod(self, module);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if self.file == "error.rs"
            && matches!(
                item.sig.ident.to_string().as_str(),
                "runtime_unavailable" | "supervisor_lost"
            )
        {
            self.replace(item.span(), "");
        } else {
            visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_item_enum(&mut self, item: &'ast syn::ItemEnum) {
        if item.attrs.iter().any(|attribute| {
            attribute.path().is_ident("derive")
                && attribute
                    .meta
                    .to_token_stream()
                    .to_string()
                    .contains("thiserror")
        }) {
            match expand_error(item) {
                Ok(implementation) => {
                    self.range(self.end(item.span()), self.end(item.span()), implementation);
                    for attribute in &item.attrs {
                        if attribute.path().is_ident("derive")
                            && attribute
                                .meta
                                .to_token_stream()
                                .to_string()
                                .contains("thiserror")
                        {
                            let tokens = attribute.parse_args_with(
                                Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                            );
                            match tokens {
                                Ok(paths) => {
                                    let kept = paths
                                        .iter()
                                        .filter(|path| {
                                            !path
                                                .to_token_stream()
                                                .to_string()
                                                .contains("thiserror")
                                        })
                                        .map(|path| path.to_token_stream().to_string())
                                        .collect::<Vec<_>>();
                                    if kept.is_empty() {
                                        let end = self.end(attribute.span());
                                        let newline =
                                            usize::from(self.source[end..].starts_with('\n'));
                                        self.range(self.start(attribute.span()), end + newline, "");
                                    } else {
                                        self.replace(
                                            attribute.span(),
                                            format!("#[derive({})]", kept.join(", ")),
                                        );
                                    }
                                }
                                Err(error) => self.error = Some(error.to_string()),
                            }
                        }
                    }
                    for variant in &item.variants {
                        for attribute in &variant.attrs {
                            if attribute.path().is_ident("error") {
                                self.replace(attribute.span(), "");
                            }
                        }
                        for field in &variant.fields {
                            for attribute in &field.attrs {
                                if attribute.path().is_ident("source")
                                    || attribute.path().is_ident("from")
                                {
                                    self.replace(attribute.span(), "");
                                }
                            }
                        }
                    }
                }
                Err(error) => self.error = Some(error.to_string()),
            }
        }
        visit::visit_item_enum(self, item);
    }
}

fn test_name(item: &syn::Item) -> Option<String> {
    let syn::Item::Fn(function) = item else {
        return None;
    };
    function
        .attrs
        .iter()
        .any(|attribute| {
            attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        })
        .then(|| function.sig.ident.to_string())
}

fn test_names(file: &syn::File) -> BTreeSet<String> {
    file.items.iter().filter_map(test_name).collect()
}

fn test_port_name<'a>(file: &str, name: &'a str) -> &'a str {
    if file == "server.rs"
        && name == "aborting_one_server_shutdown_keeps_all_clones_closed_until_later_completion"
    {
        "detaching_one_server_shutdown_keeps_all_clones_closed_until_later_completion"
    } else {
        name
    }
}

fn verify_test_port(file: &str, canonical: &str, target: &Path, records: &str) -> Result<()> {
    let parsed = syn::parse_file(canonical)?;
    let module = parsed
        .items
        .iter()
        .find_map(|item| match item {
            syn::Item::Mod(module) if module.ident == "tests" => Some(module),
            _ => None,
        })
        .ok_or("canonical manual test module is absent")?;
    let locations = BlockingVisitor::new(canonical, file, false);
    let source = locations.source_for(module.span());
    let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
    let record = records
        .lines()
        .filter(|line| !line.starts_with('#'))
        .find_map(|line| {
            let columns = line.split('\t').collect::<Vec<_>>();
            (columns.len() == 4 && columns[0] == file).then_some(columns)
        })
        .ok_or_else(|| {
            format!("missing test-port record for {file}; native source sha256 is {hash}")
        })?;
    if record[1] != hash || Path::new(record[2]) != target || record[3].is_empty() {
        return Err(format!("native tests changed in {file}; review {} and update its test-ports.tsv record (native sha256 {hash})", target.display()).into());
    }
    Ok(())
}

fn apply_edits(source: &str, mut edits: Vec<Edit>) -> Result<String> {
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut output = String::new();
    let mut cursor = 0;
    for edit in edits {
        if edit.start < cursor {
            return Err(
                format!("overlapping source transformations at byte {}", edit.start).into(),
            );
        }
        output.push_str(&source[cursor..edit.start]);
        output.push_str(&edit.replacement);
        cursor = edit.end;
    }
    output.push_str(&source[cursor..]);
    Ok(output)
}

fn trim_code_whitespace(source: &str) -> Result<String> {
    fn literals(tokens: proc_macro2::TokenStream, spans: &mut Vec<Span>) {
        for token in tokens {
            match token {
                proc_macro2::TokenTree::Literal(literal) => spans.push(literal.span()),
                proc_macro2::TokenTree::Group(group) => literals(group.stream(), spans),
                _ => {}
            }
        }
    }
    let mut spans = Vec::new();
    literals(source.parse()?, &mut spans);
    let locations = BlockingVisitor::new(source, "formatting", false);
    let ranges = spans
        .iter()
        .map(|span| locations.start(*span)..locations.end(*span))
        .collect::<Vec<_>>();
    let mut offset = 0;
    let mut output = String::new();
    for line in source.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = content.trim_end();
        let suffix = offset + trimmed.len()..offset + content.len();
        if ranges
            .iter()
            .any(|literal| literal.start < suffix.end && suffix.start < literal.end)
        {
            output.push_str(content);
        } else {
            output.push_str(trimmed);
        }
        if line.ends_with('\n') {
            output.push('\n');
        }
        offset += line.len();
    }
    Ok(output)
}

fn format_source(source: &str) -> Result<String> {
    let mut process = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let clean = trim_code_whitespace(source)?;
    process
        .stdin
        .take()
        .ok_or("rustfmt stdin unavailable")?
        .write_all(clean.as_bytes())?;
    let output = process.wait_with_output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).into_owned()).into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

struct ErrorMessage {
    format: syn::LitStr,
    arguments: Punctuated<syn::Expr, syn::Token![,]>,
}

impl Parse for ErrorMessage {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let format = input.parse()?;
        let arguments = if input.is_empty() {
            Punctuated::new()
        } else {
            input.parse::<syn::Token![,]>()?;
            Punctuated::parse_terminated(input)?
        };
        Ok(Self { format, arguments })
    }
}

fn expand_error(item: &syn::ItemEnum) -> Result<String> {
    let mut display = Vec::new();
    let mut sources = Vec::new();
    for variant in &item.variants {
        let message = variant
            .attrs
            .iter()
            .find(|attribute| attribute.path().is_ident("error"))
            .ok_or("an error variant has no display format")?
            .parse_args::<ErrorMessage>()?;
        let arguments = message.arguments.to_token_stream().to_string();
        let format = message.format.to_token_stream().to_string();
        let value = message.format.value();
        let fields = variant
            .fields
            .iter()
            .filter_map(|field| field.ident.as_ref())
            .filter(|name| {
                value.contains(&format!("{{{name}")) || arguments.contains(&name.to_string())
            })
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let pattern = match &variant.fields {
            syn::Fields::Unit => String::new(),
            syn::Fields::Named(_) if fields.is_empty() => " { .. }".to_owned(),
            syn::Fields::Named(_) => format!(" {{ {}, .. }}", fields.join(", ")),
            syn::Fields::Unnamed(_) => {
                return Err("tuple errors require an explicit expansion".into());
            }
        };
        let configuration = variant
            .attrs
            .iter()
            .filter(|attribute| attribute.path().is_ident("cfg"))
            .map(|attribute| attribute.to_token_stream().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let arguments = if arguments.is_empty() {
            String::new()
        } else {
            format!(", {arguments}")
        };
        display.push(format!(
            "{configuration}\nSelf::{}{pattern} => write!(formatter, {format}{arguments}),",
            variant.ident
        ));
        if let Some(field) = variant
            .fields
            .iter()
            .find(|field| field.ident.as_ref().is_some_and(|name| name == "source"))
        {
            let source = match &field.ty {
                syn::Type::Path(path)
                    if path
                        .path
                        .segments
                        .last()
                        .is_some_and(|segment| segment.ident == "Option") =>
                {
                    "source.as_ref().map(|error| error as &(dyn std::error::Error + 'static))"
                }
                _ => "Some(source)",
            };
            sources.push(format!(
                "{configuration}\nSelf::{} {{ source, .. }} => {source},",
                variant.ident
            ));
        }
    }
    Ok(format!(
        "\nimpl fmt::Display for {} {{ #[allow(clippy::too_many_lines, reason = \"One generated arm per error variant keeps formatting exhaustive.\")] fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {{ match self {{ {} }} }} }}\nimpl std::error::Error for {} {{ fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {{ match self {{ {} _ => None }} }} }}\n",
        item.ident,
        display.join("\n"),
        item.ident,
        sources.join("\n")
    ))
}

fn translate_docs(source: &str) -> Result<String> {
    // Code fences are handled separately from prose and ordinary comments.
    let mut lines = source.lines().map(str::to_owned).collect::<Vec<_>>();
    let mut cursor = 0;
    while cursor < lines.len() {
        let Some((prefix, content)) = doc_line(&lines[cursor]) else {
            cursor += 1;
            continue;
        };
        let Some(language) = content.trim().strip_prefix("```") else {
            if content.trim().is_empty() {
                cursor += 1;
                continue;
            }
            let prefix = prefix.to_owned();
            let first = cursor;
            let mut paragraph = Vec::new();
            while cursor < lines.len() {
                let Some((_, content)) = doc_line(&lines[cursor]) else {
                    break;
                };
                if content.trim().is_empty() || content.trim().starts_with("```") {
                    break;
                }
                paragraph.push(content.trim());
                cursor += 1;
            }
            let canonical = paragraph.join(" ");
            let replacement = doc_port(&canonical).map_or_else(
                || {
                    canonical
                        .replace("`libtmux_tokio::", "`libtmux::")
                        .replace("# Cancel safety", "# Cancellation")
                },
                str::to_owned,
            );
            if replacement != canonical {
                let translated = wrap_doc(&prefix, &replacement);
                let length = translated.len();
                lines.splice(first..cursor, translated);
                cursor = first + length;
            }
            continue;
        };
        let rust = matches!(language, "" | "rust" | "no_run" | "compile_fail" | "ignore");
        let prefix = prefix.to_owned();
        let first = cursor + 1;
        let mut last = first;
        while last < lines.len() {
            let Some((_, content)) = doc_line(&lines[last]) else {
                break;
            };
            if content.trim() == "```" {
                break;
            }
            last += 1;
        }
        if last == lines.len() || doc_line(&lines[last]).is_none() {
            return Err("unterminated Rust rustdoc fence".into());
        }
        if !rust {
            if language == "console" {
                for line in &mut lines[first..last] {
                    *line = line.replace(" -p libtmux-tokio ", " -p libtmux ");
                }
            }
            cursor = last + 1;
            continue;
        }
        let body = lines[first..last]
            .iter()
            .filter_map(|line| doc_line(line).map(|(_, content)| content.to_owned()))
            .collect::<Vec<_>>();
        let translated = translate_doc_code(&body)?;
        let translated = translated
            .into_iter()
            .map(|line| {
                if line.is_empty() {
                    prefix.trim_end().to_owned()
                } else {
                    format!("{prefix}{line}")
                }
            })
            .collect::<Vec<_>>();
        let length = translated.len();
        lines.splice(first..last, translated);
        cursor = first + length + 1;
    }
    Ok(format!("{}\n", lines.join("\n")))
}

fn doc_port(canonical: &str) -> Option<&'static str> {
    include_str!("../doc-ports.tsv")
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .find_map(|(source, target)| (source == canonical).then_some(target))
}

fn wrap_doc(prefix: &str, paragraph: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = prefix.to_owned();
    for word in paragraph.split_whitespace() {
        if line.len() > prefix.len() && line.len() + word.len() + 1 > 88 {
            lines.push(std::mem::replace(&mut line, prefix.to_owned()));
        }
        if line.len() > prefix.len() {
            line.push(' ');
        }
        line.push_str(word);
    }
    lines.push(line);
    lines
}

fn doc_line(line: &str) -> Option<(&str, &str)> {
    let indentation = line.len() - line.trim_start().len();
    let trimmed = &line[indentation..];
    if !(trimmed.starts_with("///") || trimmed.starts_with("//!")) {
        return None;
    }
    let width = indentation + 3 + usize::from(trimmed.as_bytes().get(3) == Some(&b' '));
    Some((&line[..width], &line[width..]))
}

fn translate_doc_code(lines: &[String]) -> Result<Vec<String>> {
    let mut code = Vec::new();
    let mut hidden = Vec::new();
    for line in lines {
        let (is_hidden, text) = line
            .strip_prefix("# ")
            .map_or((false, line.as_str()), |text| (true, text));
        if text.starts_with("let runtime = tokio::runtime::Builder::")
            || text == "runtime.block_on(async {"
        {
            continue;
        }
        if text.starts_with("})?") || text == "})" {
            if code
                .last()
                .is_some_and(|last: &String| last.trim_start().starts_with("Ok"))
            {
                code.pop();
                hidden.pop();
            }
            continue;
        }
        let text = text.replace(
            "// nothing; asking tmux for the windows is `async` because it does.",
            "// nothing; asking tmux for the windows blocks until the reply arrives.",
        );
        let text = if text.trim_start().starts_with("//") {
            text.replace("libtmux_tokio::", "libtmux::")
        } else {
            text
        };
        code.push(text);
        hidden.push(is_hidden);
    }
    let body = code.join("\n");
    let wrapped = format!("fn __rustdoc() {{\n{body}\n}}\n");
    let parsed = syn::parse_file(&wrapped)
        .map_err(|error| format!("{error} at {:?} in:\n{body}", error.span().start()))?;
    let mut visitor = BlockingVisitor::new(&wrapped, "rustdoc", false);
    visitor.visit_file(&parsed);
    let output = apply_edits(&wrapped, visitor.edits)?;
    let output = output
        .lines()
        .skip(1)
        .take(code.len())
        .enumerate()
        .map(|(index, line)| {
            let line = if line.trim().is_empty() && code[index].trim() == ".await" {
                ""
            } else {
                line
            };
            if hidden[index] {
                format!("# {line}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{format_source, translate, verify_source_inventory_at};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct InventoryFixture(PathBuf);

    impl InventoryFixture {
        fn new() -> std::io::Result<Self> {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "libtmux-source-inventory-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            for client in ["native", "blocking"] {
                for directory in ["src", "tests", "examples", "benches"] {
                    fs::create_dir_all(root.join(client).join(directory))?;
                }
                fs::write(
                    root.join(client).join("src/lib.rs"),
                    "pub fn capability() {}\n",
                )?;
            }
            Ok(Self(root))
        }

        fn verify(&self, inventory: &str) -> super::Result<()> {
            verify_source_inventory_at(
                &self.0.join("native"),
                &self.0.join("blocking"),
                inventory,
                &BTreeSet::new(),
            )
        }
    }

    impl Drop for InventoryFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn adding_an_unported_native_module_fails_the_inventory_gate() {
        let fixture = InventoryFixture::new().expect("an isolated source tree");
        let inventory = "src/lib.rs\tequal\tsrc/lib.rs\t-\tShared API.\n";
        fixture
            .verify(inventory)
            .expect("the complete inventory passes");
        fs::write(
            fixture.0.join("native/src/new_capability.rs"),
            "pub fn added() {}\n",
        )
        .expect("the native API grows");
        let error = fixture
            .verify(inventory)
            .expect_err("an unported module must fail");
        assert!(
            error
                .to_string()
                .contains("unrecorded [\"src/new_capability.rs\"]")
        );
    }

    #[test]
    fn changing_a_manual_canonical_body_requires_a_new_review() {
        use sha2::{Digest as _, Sha256};
        let fixture = InventoryFixture::new().expect("an isolated source tree");
        let original = fs::read(fixture.0.join("native/src/lib.rs")).expect("native source");
        let digest = format!("{:x}", Sha256::digest(&original));
        let inventory = format!("src/lib.rs\tmanual\tsrc/lib.rs\t{digest}\tManual runtime port.\n");
        fixture
            .verify(&inventory)
            .expect("the reviewed body passes");
        fs::write(
            fixture.0.join("native/src/lib.rs"),
            "pub fn capability() { changed(); }\n",
        )
        .expect("the native body changes without a rename");
        let error = fixture
            .verify(&inventory)
            .expect_err("body drift must fail");
        assert!(error.to_string().contains("native source changed"));
    }

    #[test]
    fn changing_shared_code_in_only_one_client_fails_the_gate() {
        let fixture = InventoryFixture::new().expect("an isolated source tree");
        let inventory = "src/lib.rs\tequal\tsrc/lib.rs\t-\tShared API.\n";
        fixture.verify(inventory).expect("identical code passes");
        fs::write(
            fixture.0.join("blocking/src/lib.rs"),
            "pub fn capability() { drift(); }\n",
        )
        .expect("the blocking body diverges");
        let error = fixture
            .verify(inventory)
            .expect_err("shared drift must fail");
        assert!(error.to_string().contains("stale equal source-port record"));
    }

    #[test]
    fn async_blocks_keep_their_return_and_question_mark_boundary() {
        let source = "fn outer() { let result = async { fallible()?; return Ok::<_, Error>(()); }.await; inspect(result); }";
        let output = translate(source, "test block", true)
            .expect("source translates")
            .source;
        assert!(output.contains("(|| { fallible()?; return Ok::<_, Error>(()); })()"));
    }

    #[test]
    fn unicode_spans_and_multiline_literal_spaces_are_preserved() {
        let source = "async fn run() { let name = \"日本語\"; call(name).await; let raw = b\"\nbody \n\"; }\n";
        let translated = translate(source, "unicode", false).expect("source translates");
        assert!(
            translated
                .source
                .contains("let name = \"日本語\"; call(name);")
        );
        assert!(
            format_source(&translated.source)
                .expect("source formats")
                .contains("body \n")
        );
    }

    #[test]
    fn rewriting_code_preserves_literals_and_ordinary_comments() {
        let source = r##"
// async fn and .await here document the native operation.
/* nested /* async move */ .await */
async fn command() -> Result<&'static str, Error> {
    let raw = r#"async fn .await $ % '# ;"#;
    let bytes = b".await";
    server.send(raw).await?;
    assert_eq!(server.capture().await?, bytes);
    Ok(raw)
}
"##;
        let output = translate(source, "operation.rs", false)
            .expect("valid operation")
            .source;
        assert!(output.contains("// async fn and .await here document the native operation."));
        assert!(output.contains("/* nested /* async move */ .await */"));
        assert!(output.contains("r#\"async fn .await $ % '# ;\"#"));
        assert!(output.contains("b\".await\""));
        assert!(!output.contains("async fn command"));
        assert!(output.contains("server.send(raw)?;"));
        assert!(output.contains("assert_eq!(server.capture()?, bytes);"));
    }

    #[test]
    fn scoped_creation_is_lazy_and_cleanup_is_an_ordinary_callback() {
        let source = r#"
async fn scope(&self, callback: impl AsyncFnOnce(&Item) -> Result<(), Error>) {
    scoped::run(token, "scope", async move { self.create().await },
        move |item| async move { item.kill().await }, async |item| callback(item).await).await
}
"#;
        let output = translate(source, "operation.rs", false)
            .expect("valid scope")
            .source;
        assert!(output.contains("impl FnOnce(&Item)"));
        assert!(output.contains("scoped::run(&token,"));
        assert!(output.contains("move || { self.create() }"));
        assert!(output.contains("move |item| { item.kill() }"));
        assert!(!output.contains(".await"));
    }

    #[test]
    fn doctests_convert_rust_without_interpreting_text_fences() {
        let source = r"
/// ```text
/// async fn item().await
/// ```
///
/// ```console
/// $ cargo run -p libtmux-tokio --example inspect
/// ```
///
/// # Examples
///
/// ```
/// # fn main() -> Result<(), Error> {
/// # let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
/// # runtime.block_on(async {
/// let server = libtmux_tokio::Server::new()?;
/// assert_eq!(server.sessions().await?.len(), 1);
/// server
///     .sessions()
///     .await
///     .expect(stringify!(sessions));
/// # Ok::<(), Error>(())
/// # })?;
/// # Ok(())
/// # }
/// ```
async fn item() {}
";
        let output = translate(source, "operation.rs", false)
            .expect("valid docs")
            .source;
        assert!(output.contains("/// async fn item().await"));
        assert!(output.contains("/// $ cargo run -p libtmux --example inspect"));
        assert!(output.contains("/// let server = libtmux::Server::new()?;"));
        assert!(output.contains("/// assert_eq!(server.sessions()?.len(), 1);"));
        assert!(!output.contains("///     \n"));
        assert!(!output.contains("runtime.block_on"));
        assert!(!output.contains("Ok::<(), Error>"));
    }

    #[test]
    fn raw_doc_attributes_preserve_feature_gates() {
        let source = r##"
#[cfg_attr(feature = "plan", doc = r#"```no_run
# async fn example(server: &libtmux_tokio::Server) -> Result<(), Error> {
server.sessions().await?;
# Ok(())
# }
```"#)]
pub struct Item;
"##;
        let output = translate(source, "operation.rs", false)
            .expect("valid gated docs")
            .source;
        assert!(output.contains("cfg_attr(feature = \"plan\""));
        assert!(output.contains("#  fn example(server: &libtmux::Server)"));
        assert!(output.contains("server.sessions()?;"));
    }

    #[test]
    fn macro_generated_docs_rename_the_package_without_changing_data() {
        let source = r#"
macro_rules! item {
    ($name:ident) => {
        /// ```no_run
        /// # async fn sample(server: &libtmux_tokio::Server) -> Result<(), libtmux_tokio::Error> {
        /// server.sessions().await?;
        /// # Ok(())
        /// # }
        /// ```
        #[doc = concat!("use libtmux_tokio::", stringify!($name), ";")]
        pub struct $name;
        const DATA: &str = "libtmux_tokio::unchanged";
    };
}
"#;
        let output = translate(source, "snapshot.rs", false)
            .expect("valid macro docs")
            .source;
        assert!(output.contains("concat!(\"use libtmux::\""));
        assert!(output.contains("const DATA: &str = \"libtmux_tokio::unchanged\""));
        assert!(output.contains("/// server.sessions()?;"));
    }

    #[test]
    fn fake_executors_retain_lazy_dispatch_closures() {
        let source = r"
fn execute(&self) -> DispatchFuture {
    DispatchFuture::new(async move { self.command().await })
}
fn shutdown(&self) -> ShutdownFuture {
    ShutdownFuture::new(async { Ok(()) })
}
";
        let output = translate(source, "tests", true)
            .expect("valid executor")
            .source;
        assert!(output.contains("DispatchFuture::new(move || { self.command() })"));
        assert!(output.contains("ShutdownFuture::new(|| { Ok(()) })"));
    }

    #[test]
    fn error_expansion_retains_diagnostics_sources_and_feature_gates() {
        let source = r#"
use std::fmt;
#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("bad {command}: {}", code(*exit))]
    Failed { command: String, exit: Option<i32>, private_bytes: Vec<u8> },
    #[cfg(feature = "control-mode")]
    #[error("transport failed")]
    Transport { #[source] source: Option<std::io::Error> },
}
"#;
        let output = translate(source, "error.rs", false)
            .expect("valid error")
            .source;
        assert!(!output.contains("thiserror"));
        assert!(!output.contains("#[source]"));
        assert!(output.contains("write!(formatter, \"bad {command}: {}\", code (* exit))"));
        assert!(output.contains("Self::Failed { command, exit, .. }"));
        assert!(
            output.contains(
                "source.as_ref().map(|error| error as &(dyn std::error::Error + 'static))"
            )
        );
        assert_eq!(
            output.matches("cfg (feature = \"control-mode\")").count(),
            2
        );
    }
}
