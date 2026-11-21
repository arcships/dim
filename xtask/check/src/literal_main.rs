//! `check` fails when production source names a line with a bare `"main"`
//! string literal. The root's id is `ROOT_BRANCH_ID`; `DEFAULT_BRANCH_ID` is the
//! root, standing in for the default branch at a site that does not read the
//! Session's default branch. A use of the literal that is not about lines at
//! all — SQLite's `main` schema, an HTML tag, a layout variant, a git branch
//! name — is named in `UNRELATED` with its reason.
//!
//! The two ids are defined once, each in its own language's file; a definition
//! anywhere else is refused, so no private duplicate can hold the literal.
//!
//! # The invariants
//!
//! These hold for every input, and each is checked by a test below.
//!
//! - **Rust sources are parsed, not lexed by hand.** A use is a string literal
//!   token whose value is `main`, outside every node a test-only `cfg` gates.
//!   The visitor gates a node when rustc removes it with `#[cfg]` and a
//!   literal can stand in its subtree:
//!
//!   | node | `#[cfg]` applies | can hold a literal |
//!   | --- | --- | --- |
//!   | a file's inner attributes | yes | yes |
//!   | items, `impl`, trait and foreign items | yes | yes |
//!   | statements, match arms, struct-literal fields | yes | yes |
//!   | expressions | yes | yes |
//!   | patterns (a closure parameter may carry an attribute) | yes | yes |
//!   | struct pattern fields | yes | yes |
//!   | function arguments, receiver included | yes | yes; not in a receiver's type |
//!   | bare function arguments | yes | yes |
//!   | generic parameters | yes | yes, in a const parameter's default |
//!   | a foreign item's variadic | yes | yes, as a pattern |
//!   | a bare function type's variadic | yes | no: rustc rejects a pattern there |
//!
//!   Each answer is a `rustc --edition 2024` compile test. No other `syn` type
//!   with attributes is reachable: `DeriveInput` is not parsed.
//! - **In a Vue component, only code is read:** the contents of `<script>`
//!   blocks outside HTML comments, and the template's expressions — directive
//!   and bound attribute values, and `{{ … }}` interpolations in text.
//!   Template text, plain attributes and HTML comments are not code, and a
//!   commented-out block or tag is not a block or a tag.
//! - **In a TypeScript file, only code is read:** string literals outside
//!   comments and outside template text. A template's `${ … }` is code; a `/`
//!   that opens a regular-expression literal is read as one, not as the start
//!   of a string. A `/` after a control-flow header's `)` or a block's `}`
//!   opens one, as it does after a token that cannot end an expression, while a
//!   `/` after a call's `)`, an index's `]` or an object literal's `}` divides.
//! - **The scan always terminates and never panics.** Every offset it slices at
//!   is a character boundary inside the text.
use proc_macro2::{TokenStream, TokenTree};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};
use syn::visit::Visit;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The two ids' names.
const IDS: [&str; 2] = ["ROOT_BRANCH_ID", "DEFAULT_BRANCH_ID"];

/// Each language's file for the ids' one definition, and the spelling that file
/// uses.
const HOMES: [(&str, &str); 2] = [
    ("crates/dim-agent-types/src/events.rs", "pub const "),
    ("web/src/rpc/protocol.ts", "export const "),
];

/// Production uses of `"main"` that are not about lines, each with the file's
/// path from the repository root, the expression or attribute that must span
/// the literal on its own line, and why the use is unrelated.
const UNRELATED: [(&str, &str, &str); 9] = [
    (
        "crates/dim-app-server/src/storage/sqlite.rs",
        "is_readonly(\"main\")",
        "SQLite's main schema",
    ),
    (
        "crates/dim-app-server/src/provider/persistence.rs",
        "blob_open(\"main\"",
        "SQLite's main schema",
    ),
    (
        "crates/dim-app-server/src/web/content.rs",
        "extract_tag_inner(&cleaned, \"main\")",
        "an HTML tag",
    ),
    (
        "crates/dim-app-server/src/web/content.rs",
        "| \"main\" | \"blockquote\",",
        "an HTML tag",
    ),
    (
        "web/src/features/chat/ComposerPanel.vue",
        ":data-variant=\"variant ?? 'main'\"",
        "a layout variant's default",
    ),
    (
        "web/src/features/chat/ComposerPanel.vue",
        "variant?: \"main\" | \"panel\"",
        "a layout variant's type",
    ),
    (
        "web/src/chat-ui/WebChatTimeline.vue",
        ":data-variant=\"variant ?? 'main'\"",
        "a layout variant's default",
    ),
    (
        "web/src/chat-ui/WebChatTimeline.vue",
        "variant?: \"main\" | \"panel\"",
        "a layout variant's type",
    ),
    (
        "web/src/i18n",
        "\"lines.main\": \"main\"",
        "the root line's label",
    ),
];

/// Checks every production source under `root`. Each failure names the file,
/// the line and where the id belongs.
pub fn check(root: &Path) -> Result<()> {
    let mut errors = Vec::new();
    let mut allowed = 0;
    for path in sources(root) {
        let text = fs::read_to_string(&path)?;
        let file = relative(root, &path);
        allowed += findings(&file, &text, &mut errors)?;
    }
    if errors.is_empty() {
        println!("literal main passed ({allowed} unrelated uses)");
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// The uses `text` has, whose repository-relative path is `file`, by the rule a
/// use earns; answers how many uses `UNRELATED` allows and adds every other use
/// to `errors`. A Rust source the parser cannot read is an error naming the
/// file: the rule fails closed rather than skipping a file it cannot see into.
fn findings(file: &str, text: &str, errors: &mut Vec<String>) -> Result<usize> {
    let mut allowed = 0;
    let offsets = match uses(file, text) {
        Ok(offsets) => offsets,
        Err(error) => {
            errors.push(error.to_string());
            return Ok(0);
        }
    };
    for offset in offsets {
        let line = line_at(text, offset);
        let source = text.lines().nth(line.saturating_sub(1)).unwrap_or_default();
        if names_the_id(file, source) {
            continue;
        }
        if defines_an_id(source) {
            errors.push(format!(
                "{file}:{line}: a branch id is defined once, in {} or {}; \
                 name the definition's home in `HOMES` instead of copying it",
                HOMES[0].0, HOMES[1].0
            ));
            continue;
        }
        if unrelated(file, text, offset) {
            allowed += 1;
            continue;
        }
        errors.push(format!(
            "{file}:{line}: `\"main\"` names neither the root nor the default branch; \
             write `ROOT_BRANCH_ID` or `DEFAULT_BRANCH_ID`, or name the use in `UNRELATED`"
        ));
    }
    Ok(allowed)
}

/// The byte offsets of `text`'s `main` literals that are code, by the file's
/// spelling. Each offset is the literal's opening quote.
fn uses(file: &str, text: &str) -> Result<Vec<usize>> {
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "rs" => rust_uses(file, text).map_err(Into::into),
        "vue" => Ok(vue_uses(text)),
        _ => Ok(javascript_uses(text)),
    }
}

/// The 1-based line `offset` sits on. Every caller takes `offset` from a scan
/// that reports character boundaries.
fn line_at(text: &str, offset: usize) -> usize {
    text[..offset].matches('\n').count() + 1
}

/// The repository-relative spelling of `path` under `root`, with `/` as its
/// only separator: a Windows checkout spells the same file with `\`, while
/// `HOMES` and `UNRELATED` are written with `/`.
fn relative(root: &Path, path: &Path) -> String {
    normalize(&path.strip_prefix(root).unwrap_or(path).to_string_lossy())
}

/// `path` with every separator a platform may write replaced by `/`.
fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Whether `line` of `file` is one of the ids' definitions at its own home, in
/// the spelling that home uses: `pub const NAME: &str = "main"` in Rust and
/// `export const NAME = "main"` in TypeScript.
fn names_the_id(file: &str, line: &str) -> bool {
    HOMES
        .iter()
        .any(|(home, spelling)| file == *home && declares(line, spelling))
}

/// Whether `line` defines one of the ids, wherever it is written.
fn defines_an_id(line: &str) -> bool {
    ["pub const ", "export const ", "const "]
        .iter()
        .any(|spelling| declares(line, spelling))
}

/// Whether `line` declares one of the ids with `spelling`, which leads the line.
fn declares(line: &str, spelling: &str) -> bool {
    let Some(declared) = line.trim_start().strip_prefix(spelling) else {
        return false;
    };
    IDS.iter().any(|name| {
        declared.starts_with(name)
            && !declared[name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
    })
}

/// Whether an allowlist entry covers the literal at `offset` in `text`: the
/// entry's file must match, and one occurrence of its text on the literal's own
/// line must span the literal's position. An entry names the expression or
/// attribute that holds the literal, so a second literal on the same line is
/// not covered by it.
fn unrelated(file: &str, text: &str, offset: usize) -> bool {
    let start = text[..offset].rfind('\n').map_or(0, |newline| newline + 1);
    let line = text[start..].split('\n').next().unwrap_or_default();
    let column = offset - start;
    UNRELATED.iter().any(|(path, entry, _)| {
        file.starts_with(path)
            && line
                .match_indices(entry)
                .any(|(at, found)| at <= column && column < at + found.len())
    })
}

/// Every production source the rule reads: each crate's `src`, and the Web
/// workspace's `src`.
fn sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(root.join("crates")) {
        for entry in entries.flatten() {
            let directory = entry.path().join("src");
            if directory.is_dir() {
                collect(&directory, &mut files, Kind::Rust);
            }
        }
    }
    collect(&root.join("web/src"), &mut files, Kind::Web);
    files.sort();
    files
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Rust,
    Web,
}

/// Collects the production files under `directory`, outside a `tests`
/// directory: a test module's sources are the root's stored id and are not
/// the rule's business.
fn collect(directory: &Path, files: &mut Vec<PathBuf>, kind: Kind) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
        else {
            continue;
        };
        if path.is_dir() {
            if name != "tests" {
                collect(&path, files, kind);
            }
        } else if is_production(&name, kind) {
            files.push(path);
        }
    }
}

/// Whether `name` is a production file the rule reads of its kind: a Rust file
/// that is not a test module, a Web source that is not a test.
fn is_production(name: &str, kind: Kind) -> bool {
    let is_test = name.contains(".test.") || name == "tests.rs";
    !is_test
        && match kind {
            Kind::Rust => name.ends_with(".rs"),
            Kind::Web => {
                name.ends_with(".ts")
                    || name.ends_with(".vue")
                    || name.ends_with(".mjs")
                    || name.ends_with(".js")
            }
        }
}

// ---------------------------------------------------------------- Rust

/// The byte offsets of the `main` literals in `text` that are production.
fn rust_uses(file: &str, text: &str) -> std::result::Result<Vec<usize>, String> {
    let parsed = syn::parse_file(text).map_err(|error| {
        format!("{file}: the literal-main rule cannot parse this source: {error}")
    })?;
    let mut visitor = RustLiterals {
        offsets: Vec::new(),
        gated: false,
    };
    visitor.visit_file(&parsed);
    Ok(visitor.offsets)
}

/// Collects the byte offsets of the `main` literals that are production, and
/// nothing from an item a test-only `cfg` gates. The gate is read from the
/// parsed attributes, so a doc line that mentions `#[cfg(test)]` is not a gate:
/// it is an attribute whose body is a doc string, and `cfg` is not its path.
struct RustLiterals {
    offsets: Vec<usize>,
    /// Whether the node being visited is inside a test-only `cfg`, which every
    /// literal below it inherits.
    gated: bool,
}

/// Runs `inner` for the node, with `gated` set for the node and restored after
/// it: the gate a node inherits is its own attributes' or its parent's.
fn gated(
    this: &mut RustLiterals,
    attributes: &[syn::Attribute],
    inner: impl FnOnce(&mut RustLiterals, bool),
) {
    let outer = this.gated;
    this.gated = outer || test_only(attributes);
    inner(this, this.gated);
    this.gated = outer;
}

impl<'ast> Visit<'ast> for RustLiterals {
    fn visit_file(&mut self, node: &'ast syn::File) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_file(this, node);
            }
        });
    }

    fn visit_item(&mut self, node: &'ast syn::Item) {
        gated(self, item_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_item(this, node);
            }
        });
    }

    fn visit_impl_item(&mut self, node: &'ast syn::ImplItem) {
        gated(self, impl_item_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_impl_item(this, node);
            }
        });
    }

    fn visit_trait_item(&mut self, node: &'ast syn::TraitItem) {
        gated(self, trait_item_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_trait_item(this, node);
            }
        });
    }

    fn visit_stmt(&mut self, node: &'ast syn::Stmt) {
        gated(self, stmt_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_stmt(this, node);
            }
        });
    }

    fn visit_expr(&mut self, node: &'ast syn::Expr) {
        gated(self, expr_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_expr(this, node);
            }
        });
    }

    fn visit_pat(&mut self, node: &'ast syn::Pat) {
        gated(self, pat_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_pat(this, node);
            }
        });
    }

    fn visit_fn_arg(&mut self, node: &'ast syn::FnArg) {
        gated(self, fn_arg_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_fn_arg(this, node);
            }
        });
    }

    fn visit_bare_fn_arg(&mut self, node: &'ast syn::BareFnArg) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_bare_fn_arg(this, node);
            }
        });
    }

    fn visit_generic_param(&mut self, node: &'ast syn::GenericParam) {
        gated(self, generic_param_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_generic_param(this, node);
            }
        });
    }

    fn visit_variadic(&mut self, node: &'ast syn::Variadic) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_variadic(this, node);
            }
        });
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_arm(this, node);
            }
        });
    }

    fn visit_field_value(&mut self, node: &'ast syn::FieldValue) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_field_value(this, node);
            }
        });
    }

    fn visit_field(&mut self, node: &'ast syn::Field) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_field(this, node);
            }
        });
    }

    fn visit_field_pat(&mut self, node: &'ast syn::FieldPat) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_field_pat(this, node);
            }
        });
    }

    fn visit_variant(&mut self, node: &'ast syn::Variant) {
        gated(self, &node.attrs, |this, gated| {
            if !gated {
                syn::visit::visit_variant(this, node);
            }
        });
    }

    fn visit_foreign_item(&mut self, node: &'ast syn::ForeignItem) {
        gated(self, foreign_item_attributes(node), |this, gated| {
            if !gated {
                syn::visit::visit_foreign_item(this, node);
            }
        });
    }

    fn visit_attribute(&mut self, _: &'ast syn::Attribute) {
        // A doc comment is an attribute holding text, and a doc line that
        // writes `"main"` in an example is documentation, not a use.
    }

    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        if !self.gated && node.value() == "main" {
            self.offsets.push(node.span().byte_range().start);
        }
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if !self.gated {
            self.offsets.extend(macro_offsets(&node.tokens));
        }
        syn::visit::visit_macro(self, node);
    }
}

/// The attributes an item may carry, whichever item it is.
fn item_attributes(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes an `impl` block's item may carry.
fn impl_item_attributes(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(item) => &item.attrs,
        syn::ImplItem::Fn(item) => &item.attrs,
        syn::ImplItem::Macro(item) => &item.attrs,
        syn::ImplItem::Type(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes a trait's item may carry.
fn trait_item_attributes(item: &syn::TraitItem) -> &[syn::Attribute] {
    match item {
        syn::TraitItem::Const(item) => &item.attrs,
        syn::TraitItem::Fn(item) => &item.attrs,
        syn::TraitItem::Macro(item) => &item.attrs,
        syn::TraitItem::Type(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes a foreign item may carry.
fn foreign_item_attributes(item: &syn::ForeignItem) -> &[syn::Attribute] {
    match item {
        syn::ForeignItem::Fn(item) => &item.attrs,
        syn::ForeignItem::Static(item) => &item.attrs,
        syn::ForeignItem::Type(item) => &item.attrs,
        syn::ForeignItem::Macro(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes a statement may carry: an item's, a local's or a macro
/// statement's. An expression statement's own gate is its expression's, read
/// in `visit_expr`.
fn stmt_attributes(statement: &syn::Stmt) -> &[syn::Attribute] {
    match statement {
        syn::Stmt::Item(item) => item_attributes(item),
        syn::Stmt::Local(item) => &item.attrs,
        syn::Stmt::Macro(item) => &item.attrs,
        syn::Stmt::Expr(_, _) => &[],
    }
}

/// The outer attributes a pattern may carry, whichever pattern it is.
fn pat_attributes(pattern: &syn::Pat) -> &[syn::Attribute] {
    match pattern {
        syn::Pat::Const(item) => &item.attrs,
        syn::Pat::Ident(item) => &item.attrs,
        syn::Pat::Lit(item) => &item.attrs,
        syn::Pat::Macro(item) => &item.attrs,
        syn::Pat::Or(item) => &item.attrs,
        syn::Pat::Paren(item) => &item.attrs,
        syn::Pat::Path(item) => &item.attrs,
        syn::Pat::Range(item) => &item.attrs,
        syn::Pat::Reference(item) => &item.attrs,
        syn::Pat::Rest(item) => &item.attrs,
        syn::Pat::Slice(item) => &item.attrs,
        syn::Pat::Struct(item) => &item.attrs,
        syn::Pat::Tuple(item) => &item.attrs,
        syn::Pat::TupleStruct(item) => &item.attrs,
        syn::Pat::Type(item) => &item.attrs,
        syn::Pat::Wild(item) => &item.attrs,
        _ => &[],
    }
}

/// The attributes a function argument may carry, receiver or typed argument.
fn fn_arg_attributes(argument: &syn::FnArg) -> &[syn::Attribute] {
    match argument {
        syn::FnArg::Receiver(item) => &item.attrs,
        syn::FnArg::Typed(item) => &item.attrs,
    }
}

/// The attributes a generic parameter may carry, whichever parameter it is.
fn generic_param_attributes(parameter: &syn::GenericParam) -> &[syn::Attribute] {
    match parameter {
        syn::GenericParam::Lifetime(item) => &item.attrs,
        syn::GenericParam::Type(item) => &item.attrs,
        syn::GenericParam::Const(item) => &item.attrs,
    }
}

/// The outer attributes an expression may carry, whichever expression it is.
fn expr_attributes(expression: &syn::Expr) -> &[syn::Attribute] {
    match expression {
        syn::Expr::Array(item) => &item.attrs,
        syn::Expr::Assign(item) => &item.attrs,
        syn::Expr::Async(item) => &item.attrs,
        syn::Expr::Await(item) => &item.attrs,
        syn::Expr::Binary(item) => &item.attrs,
        syn::Expr::Block(item) => &item.attrs,
        syn::Expr::Break(item) => &item.attrs,
        syn::Expr::Call(item) => &item.attrs,
        syn::Expr::Cast(item) => &item.attrs,
        syn::Expr::Closure(item) => &item.attrs,
        syn::Expr::Const(item) => &item.attrs,
        syn::Expr::Continue(item) => &item.attrs,
        syn::Expr::Field(item) => &item.attrs,
        syn::Expr::ForLoop(item) => &item.attrs,
        syn::Expr::Group(item) => &item.attrs,
        syn::Expr::If(item) => &item.attrs,
        syn::Expr::Index(item) => &item.attrs,
        syn::Expr::Infer(item) => &item.attrs,
        syn::Expr::Let(item) => &item.attrs,
        syn::Expr::Lit(item) => &item.attrs,
        syn::Expr::Loop(item) => &item.attrs,
        syn::Expr::Macro(item) => &item.attrs,
        syn::Expr::Match(item) => &item.attrs,
        syn::Expr::MethodCall(item) => &item.attrs,
        syn::Expr::Paren(item) => &item.attrs,
        syn::Expr::Path(item) => &item.attrs,
        syn::Expr::Range(item) => &item.attrs,
        syn::Expr::RawAddr(item) => &item.attrs,
        syn::Expr::Reference(item) => &item.attrs,
        syn::Expr::Repeat(item) => &item.attrs,
        syn::Expr::Return(item) => &item.attrs,
        syn::Expr::Struct(item) => &item.attrs,
        syn::Expr::Try(item) => &item.attrs,
        syn::Expr::TryBlock(item) => &item.attrs,
        syn::Expr::Tuple(item) => &item.attrs,
        syn::Expr::Unary(item) => &item.attrs,
        syn::Expr::Unsafe(item) => &item.attrs,
        syn::Expr::While(item) => &item.attrs,
        syn::Expr::Yield(item) => &item.attrs,
        syn::Expr::Verbatim(_) => &[],
        _ => &[],
    }
}

/// Whether any of `attributes` is a `cfg` that holds only when `test` does.
fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && match &attribute.meta {
                syn::Meta::List(list) => list
                    .parse_args_with(
                        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                    )
                    .is_ok_and(|nested| match nested.len() {
                        1 => nested.first().is_some_and(test_only_meta),
                        _ => false,
                    }),
                _ => false,
            }
    })
}

/// Whether a `cfg` predicate holds only when `test` does. `all(…)` holds when
/// every part does, so one `test` part makes the whole test-only; `any(…)`
/// holds when one part does, so it is test-only only when every part is;
/// `not(…)` and a `key = "value"` predicate are never test-only, because they
/// can hold without `test`.
fn test_only_meta(meta: &syn::Meta) -> bool {
    match meta {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) => {
            let Ok(nested) = list.parse_args_with(
                syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
            ) else {
                return false;
            };
            if list.path.is_ident("all") {
                nested.iter().any(test_only_meta)
            } else if list.path.is_ident("any") {
                !nested.is_empty() && nested.iter().all(test_only_meta)
            } else {
                false
            }
        }
        syn::Meta::NameValue(_) => false,
    }
}

/// The byte offsets of the `main` literals in a macro's tokens, read through the
/// same parser a source literal goes through: `json!({"branchId": "main"})` and
/// a `format!` argument are uses like any other.
fn macro_offsets(tokens: &TokenStream) -> Vec<usize> {
    tokens
        .clone()
        .into_iter()
        .flat_map(|tree| match tree {
            TokenTree::Literal(literal) => {
                // The value comes from a parse of the token's text, which is a
                // new span; the position comes from the token itself, which is
                // the one the source wrote.
                let value = syn::parse_str::<syn::LitStr>(&literal.to_string());
                match value.is_ok_and(|literal| literal.value() == "main") {
                    true => vec![literal.span().byte_range().start],
                    false => Vec::new(),
                }
            }
            TokenTree::Group(group) => macro_offsets(&group.stream()),
            _ => Vec::new(),
        })
        .collect()
}

// ----------------------------------------------------------- Vue and TS

/// The byte offsets of the `main` literals in a Vue component's code: the
/// contents of its `<script>` blocks, and the expressions of its template.
fn vue_uses(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    for block in script_blocks(text) {
        found.extend(
            javascript_uses(&text[block.clone()])
                .into_iter()
                .map(|offset| offset + block.start),
        );
    }
    for value in expression_values(text) {
        found.extend(
            javascript_uses(&text[value.clone()])
                .into_iter()
                .map(|offset| offset + value.start),
        );
    }
    found.sort_unstable();
    found.dedup();
    found
}

/// The kind of a top-level block a Vue component may hold.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Script,
    Template,
}

/// Each top-level `<script …>` and `<template …>` block in `text`, with its
/// content's byte range, found in one pass that skips an HTML comment whole
/// before it looks for a tag: a commented-out block is markup, not code. A
/// block's own content is not scanned for a tag, so a `<script>` written in
/// markup is not a block either.
fn blocks(text: &str) -> Vec<(Block, std::ops::Range<usize>)> {
    let mut blocks = Vec::new();
    let mut at = 0;
    while let Some(character) = text[at..].chars().next() {
        let rest = &text[at..];
        if rest.starts_with("<!--") {
            // `<!--` and `-->` are ASCII, so the offset past them is a boundary.
            at = match rest.find("-->") {
                Some(end) => at + end + 3,
                None => text.len(),
            };
            continue;
        }
        let kind = match tag_at(rest) {
            Some(kind) => kind,
            None => {
                at += character.len_utf8();
                continue;
            }
        };
        let Some(close) = rest.find('>') else {
            break;
        };
        let start = at + close + 1;
        let (end, next) = match kind {
            Block::Script => {
                let end = text[start..]
                    .find("</script>")
                    .map_or(text.len(), |end| start + end);
                (end, (end + "</script>".len()).min(text.len()))
            }
            Block::Template => match text[start..].rfind("</template>") {
                Some(end) => (
                    start + end,
                    (start + end + "</template>".len()).min(text.len()),
                ),
                // An unclosed `<template>` is not an element: skip its tag and
                // keep looking for a block.
                None => {
                    at = start;
                    continue;
                }
            },
        };
        blocks.push((kind, start..end));
        at = next;
    }
    blocks
}

/// The kind of block the tag at the start of `rest` opens, `None` when `rest`
/// opens neither.
fn tag_at(rest: &str) -> Option<Block> {
    for (tag, kind) in [("<script", Block::Script), ("<template", Block::Template)] {
        if rest.strip_prefix(tag).is_some_and(|after| {
            after.is_empty()
                || after.starts_with(|c: char| c.is_ascii_whitespace() || matches!(c, '>' | '/'))
        }) {
            return Some(kind);
        }
    }
    None
}

/// The byte range of each `<script …>` block in `text`.
fn script_blocks(text: &str) -> Vec<std::ops::Range<usize>> {
    blocks(text)
        .into_iter()
        .filter_map(|(kind, block)| (kind == Block::Script).then_some(block))
        .collect()
}

/// The byte ranges of a component's template expressions: the values of its
/// directive and bound attributes, and its `{{ … }}` interpolations in text.
/// Each one is a range of code. A plain attribute's value is not an expression,
/// and neither is the text around an interpolation: Vue interpolates only in
/// text nodes.
fn expression_values(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut values = Vec::new();
    let Some(body) = template_body(text) else {
        return values;
    };
    let mut at = body.start;
    while at < body.end {
        let rest = &text[at..body.end];
        if rest.starts_with("<!--") {
            // An HTML comment holds no expression, and it is skipped whole:
            // `<!--` and `-->` are ASCII, so the offsets past them are
            // boundaries.
            at = match rest.find("-->") {
                Some(end) => at + end + 3,
                None => body.end,
            };
        } else if rest.starts_with("{{") {
            // `{{` is ASCII, so the two bytes past its boundary start a
            // boundary.
            let content = at + 2;
            let Some(close) = text[content..body.end].find("}}") else {
                break;
            };
            values.push(content..content + close);
            at = content + close + 2;
        } else if rest.starts_with('<') {
            // A tag is not text: its directive and bound attributes are read,
            // and an interpolation written inside one is skipped with the tag.
            // A `<` that opens no tag costs one step.
            at = tag_attributes(text, at + 1, body.end, &mut values);
        } else {
            // A character of text; its own length keeps `at` a boundary.
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    values
}

/// The range of `text`'s `<template>` element's content, `None` when it has
/// none.
fn template_body(text: &str) -> Option<std::ops::Range<usize>> {
    blocks(text)
        .into_iter()
        .find_map(|(kind, block)| (kind == Block::Template).then_some(block))
}

/// Reads the attributes of the tag whose name begins at `cursor` in `text`,
/// pushing the value range of each directive one, and answers where the scan
/// resumes. `end` bounds the template: every read stops there, so a `<` that
/// opens no tag costs one step and never holds the scan. Every offset it
/// returns is a boundary of `text[..end]`, which is a boundary of `text`.
fn tag_attributes(
    text: &str,
    cursor: usize,
    end: usize,
    values: &mut Vec<std::ops::Range<usize>>,
) -> usize {
    let mut cursor = cursor;
    let Some(first) = text.get(cursor..end).and_then(|rest| rest.chars().next()) else {
        return end;
    };
    if !first.is_ascii_alphabetic() && !matches!(first, '/' | '!' | '?') {
        // A `<` that opens no tag, such as a comparison in an expression. The
        // scan resumes at `cursor`, which is a boundary: it is the offset just
        // past the ASCII `<` the caller matched.
        return cursor;
    }
    // The tag's name, up to whitespace, `>`, `/` or the end.
    while let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) {
        if character.is_ascii_whitespace() || matches!(character, '>' | '/') {
            break;
        }
        cursor += character.len_utf8();
    }
    loop {
        while let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) {
            if !character.is_ascii_whitespace() {
                break;
            }
            cursor += character.len_utf8();
        }
        let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) else {
            return end;
        };
        match character {
            '>' => return cursor + 1,
            '/' => {
                cursor += 1;
                continue;
            }
            _ => {}
        }
        let name = cursor;
        while let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) {
            if character.is_ascii_whitespace() || matches!(character, '=' | '>' | '/') {
                break;
            }
            cursor += character.len_utf8();
        }
        if cursor == name {
            // Nothing consumed: a character that starts neither a name nor a
            // value. Step over it by its own length so the scan always moves.
            cursor += character.len_utf8();
            continue;
        }
        let directive = {
            let name = &text[name..cursor];
            name.starts_with("v-") || name.starts_with([':', '@', '#'])
        };
        while let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) {
            if !character.is_ascii_whitespace() {
                break;
            }
            cursor += character.len_utf8();
        }
        if text.get(cursor..end).and_then(|rest| rest.chars().next()) != Some('=') {
            continue;
        }
        cursor += 1;
        while let Some(character) = text.get(cursor..end).and_then(|rest| rest.chars().next()) {
            if !character.is_ascii_whitespace() {
                break;
            }
            cursor += character.len_utf8();
        }
        let Some(quote @ ('"' | '\'')) = text.get(cursor..end).and_then(|rest| rest.chars().next())
        else {
            continue;
        };
        let value = cursor + quote.len_utf8();
        let close = text[value..end]
            .find(quote)
            .map_or(end, |close| value + close);
        if directive {
            values.push(value..close);
        }
        cursor = (close + quote.len_utf8()).min(end);
    }
}

/// The byte offsets of the `main` literals in a JavaScript or TypeScript
/// `text`, outside its comments and its template text. Every offset is the
/// opening quote of the literal, which is a boundary.
fn javascript_uses(text: &str) -> Vec<usize> {
    let mut found = Vec::new();
    code(text, false, &mut found);
    found
}

/// The keywords after which a `/` begins a regular-expression literal rather
/// than dividing: none of them can end an expression, each is followed by an
/// expression or a statement body. `await`, `delete`, `typeof`, `void` and
/// `yield` take a unary operand, `new` its constructor's member expression,
/// `in`, `instanceof` and `of` a right operand, `case` its expression, `return`
/// and `throw` their value, `else` and `do` a statement body, which a `/`
/// begins as an expression statement, `default` the expression of
/// `export default`, and `extends` a class's heritage.
const KEYWORDS: [&str; 16] = [
    "await",
    "case",
    "default",
    "delete",
    "do",
    "else",
    "extends",
    "in",
    "instanceof",
    "new",
    "of",
    "return",
    "throw",
    "typeof",
    "void",
    "yield",
];

/// The token before the current one, as far as an opener's role needs it.
#[derive(Clone, Copy)]
enum Before<'a> {
    /// The start of the region, before any token.
    Start,
    /// An identifier, a keyword or a number, as written.
    Word(&'a str),
    /// `;`.
    Semicolon,
    /// `{`.
    Brace,
    /// `}`.
    CloseBrace,
    /// `)`.
    CloseParen,
    /// `=>`.
    Arrow,
    /// Any other token: an opener other than `{`, a separator or an operator.
    Other,
}

/// What the token that closes an opener ends, which decides whether a `/`
/// after it opens a regular-expression literal or divides.
#[derive(Clone, Copy)]
enum Ends {
    /// An expression: a call's `)` or an object literal's `}`.
    Expression,
    /// A control-flow header's `)` or a block's `}`.
    HeaderOrBlock,
}

/// Whether a `(` after `before` opens the header of `if`, `while`, `for` or
/// `with` rather than a call, a grouping or a parameter list.
fn header(before: Before<'_>) -> bool {
    matches!(before, Before::Word("if" | "while" | "for" | "with"))
}

/// Whether a `{` after `before` opens a block rather than an object literal.
/// A block follows the start of the region, `;`, `{`, `}`, `)`, `=>`, `else`,
/// `do`, `try`, `finally`, or an identifier or keyword that `KEYWORDS` does not
/// name; anything else is an object literal, among it `return` and `(`.
fn block(before: Before<'_>) -> bool {
    match before {
        Before::Start
        | Before::Semicolon
        | Before::Brace
        | Before::CloseBrace
        | Before::CloseParen
        | Before::Arrow => true,
        Before::Word(word) => {
            matches!(word, "else" | "do" | "try" | "finally") || !KEYWORDS.contains(&word)
        }
        Before::Other => false,
    }
}

/// Reads `text` as code, pushing into `found` the offset of every `main`
/// literal, and answers how many bytes it read: all of `text`, or, when `brace`
/// is set, the bytes up to and including the `}` that closes the `${ … }` it
/// reads for.
///
/// A string's text is not code, and neither is a template's; a template's
/// `${ … }` is, and is read by this same function, so an interpolation can nest
/// braces, strings, comments, templates and regular-expression literals. A `/`
/// begins a regular-expression literal when the token before it cannot end an
/// expression, and divides otherwise. Whitespace changes no token, so it
/// neither ends an expression nor hides the token before an opener.
///
/// A `)` or `}` ends an expression unless it closes an opener the stack
/// remembers as a control-flow header or a block. A `(` after `if`, `while`,
/// `for` or `with` opens such a header; a `{` after the start of the region,
/// `;`, `{`, `}`, `)`, `=>`, `else`, `do`, `try`, `finally` or a word that
/// `KEYWORDS` does not name opens a block; any other `(` or `{` ends an
/// expression. Every branch moves the scan forward, and a literal that never
/// closes runs to the end of `text`, so the scan always ends.
fn code(text: &str, brace: bool, found: &mut Vec<usize>) -> usize {
    let mut at = 0;
    let mut depth = 0usize;
    let mut expression = true;
    let mut before = Before::Start;
    let mut open = Vec::new();
    while let Some(character) = text[at..].chars().next() {
        let next = at + character.len_utf8();
        if let Some(length) = comment(text, at) {
            at = (at + length).min(text.len());
        } else if character.is_whitespace() {
            at = next;
        } else if let Some((body, length)) = string(text, at) {
            if text[at + 1..at + 1 + body] == *"main" {
                found.push(at);
            }
            at = (at + length).min(text.len());
            expression = false;
            before = Before::Other;
        } else if character == '`' {
            at = (at + template(text, at, found)).min(text.len());
            expression = false;
            before = Before::Other;
        } else if character == '/' && expression {
            match regex(text, at) {
                Some(length) => {
                    at = (at + length).min(text.len());
                    expression = false;
                }
                None => {
                    at = next;
                    expression = true;
                }
            }
            before = Before::Other;
        } else if character == '(' {
            open.push(if header(before) {
                Ends::HeaderOrBlock
            } else {
                Ends::Expression
            });
            at = next;
            expression = true;
            before = Before::Other;
        } else if character == '{' {
            open.push(if block(before) {
                Ends::HeaderOrBlock
            } else {
                Ends::Expression
            });
            depth += 1;
            at = next;
            expression = true;
            before = Before::Brace;
        } else if character == '}' && brace && depth == 0 {
            return next;
        } else if character == '}' {
            depth = depth.saturating_sub(1);
            at = next;
            expression = matches!(open.pop(), Some(Ends::HeaderOrBlock));
            before = Before::CloseBrace;
        } else if character == ')' {
            at = next;
            expression = matches!(open.pop(), Some(Ends::HeaderOrBlock));
            before = Before::CloseParen;
        } else if character == ']' {
            at = next;
            expression = false;
            before = Before::Other;
        } else if character.is_alphanumeric() || matches!(character, '_' | '$' | '.') {
            let mut end = next;
            while let Some(character) = text[end..].chars().next() {
                if !(character.is_alphanumeric() || matches!(character, '_' | '$' | '.')) {
                    break;
                }
                end += character.len_utf8();
            }
            let word = &text[at..end];
            before = Before::Word(word);
            expression = KEYWORDS.contains(&word);
            at = end;
        } else if character == '=' && text[next..].starts_with('>') {
            at = next + 1;
            expression = true;
            before = Before::Arrow;
        } else {
            at = next;
            expression = true;
            before = if character == ';' {
                Before::Semicolon
            } else {
                Before::Other
            };
        }
    }
    at
}

/// The length of the comment that starts at `at`, `None` when one does not.
fn comment(text: &str, at: usize) -> Option<usize> {
    let rest = text.get(at..)?;
    if let Some(body) = rest.strip_prefix("//") {
        return Some(2 + body.find('\n').unwrap_or(body.len()));
    }
    let body = rest.strip_prefix("/*")?;
    Some(2 + body.find("*/").map_or(body.len(), |end| end + 2))
}

/// The string literal that starts at `at`, as the byte length of its body and
/// of the whole literal, `None` when one does not start there. `'…'` and `"…"`
/// are read; a backslash quotes the character after it, so `"\""` is one
/// literal rather than two.
fn string(text: &str, at: usize) -> Option<(usize, usize)> {
    let rest = text.get(at..)?;
    let quote = rest.chars().next().filter(|q| matches!(q, '\'' | '"'))?;
    let body = &rest[quote.len_utf8()..];
    let mut characters = body.char_indices();
    while let Some((offset, character)) = characters.next() {
        if character == '\\' {
            characters.next();
        } else if character == quote {
            return Some((offset, quote.len_utf8() + offset + quote.len_utf8()));
        }
    }
    Some((body.len(), body.len() + quote.len_utf8()))
}

/// The length of the template literal that starts with the backtick at `at`,
/// reading each `${ … }` of its text as code and pushing the literals found
/// there into `found`. The text between interpolations is not code, so a
/// template with no interpolation is a use only when its whole text is `main`,
/// as a string would be. A template that never closes runs to the end of `text`.
fn template(text: &str, at: usize, found: &mut Vec<usize>) -> usize {
    let mut cursor = at + 1;
    let mut interpolated = false;
    while let Some(character) = text[cursor..].chars().next() {
        match character {
            '`' => {
                if !interpolated && &text[at + 1..cursor] == "main" {
                    found.push(at);
                }
                return cursor + 1 - at;
            }
            '\\' => {
                cursor += 1;
                if let Some(escaped) = text[cursor..].chars().next() {
                    cursor += escaped.len_utf8();
                }
            }
            '$' if text
                .get(cursor + 1..)
                .is_some_and(|rest| rest.starts_with('{')) =>
            {
                interpolated = true;
                let open = cursor + 2;
                let mut nested = Vec::new();
                let length = code(&text[open..], true, &mut nested);
                found.extend(nested.into_iter().map(|offset| offset + open));
                cursor = open + length;
            }
            _ => cursor += character.len_utf8(),
        }
    }
    text.len() - at
}

/// The length of the regular-expression literal that starts with the `/` at
/// `at`, `None` when no literal starts there. The body ends at an unescaped `/`
/// outside a `[...]` class and cannot cross a line, so a `/` with neither is
/// division. A literal's flags follow its closing `/`.
fn regex(text: &str, at: usize) -> Option<usize> {
    let mut cursor = at + 1;
    let mut class = false;
    while let Some(character) = text[cursor..].chars().next() {
        match character {
            '\n' | '\r' => return None,
            '\\' => {
                cursor += 1;
                if let Some(escaped) = text[cursor..].chars().next() {
                    cursor += escaped.len_utf8();
                }
                continue;
            }
            '[' => class = true,
            ']' => class = false,
            '/' if !class => {
                cursor += 1;
                while let Some(flag) = text[cursor..]
                    .chars()
                    .next()
                    .filter(|character| character.is_alphanumeric())
                {
                    cursor += flag.len_utf8();
                }
                return Some(cursor - at);
            }
            _ => {}
        }
        cursor += character.len_utf8();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dim-xtask-literal-main-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (path, text) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn refusal(root: &Path) -> String {
        let error = check(root).unwrap_err().to_string();
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    /// The failures one Rust source has, and a failure if the scan has not
    /// answered in ten seconds: a regression fails the test instead of hanging
    /// CI.
    fn rust_findings(text: &str) -> Vec<String> {
        bounded("crates/example/src/lib.rs", text)
    }

    /// The failures one Vue component has, bounded the same way.
    fn vue_findings(text: &str) -> Vec<String> {
        bounded("web/src/features/chat/LinesTree.vue", text)
    }

    /// The failures `text` has at `file`, and a panic rather than a hang when
    /// the scan never answers.
    fn bounded(file: &str, text: &str) -> Vec<String> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let (file, text) = (file.to_owned(), text.to_owned());
        let scan = std::thread::spawn(move || {
            let mut errors = Vec::new();
            let _ = findings(&file, &text, &mut errors);
            let _ = sender.send(errors);
        });
        let errors = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap_or_else(|error| match error {
                std::sync::mpsc::RecvTimeoutError::Timeout => {
                    panic!("the scan never returned")
                }
                std::sync::mpsc::RecvTimeoutError::Disconnected => {
                    panic!("the scan panicked instead of answering")
                }
            });
        scan.join().unwrap();
        errors
    }

    #[test]
    fn a_bare_main_in_production_is_refused_with_its_line() {
        let root = tree(
            "bare",
            &[(
                "crates/example/src/lib.rs",
                "fn branch() -> &'static str {\n    \"main\"\n}\n",
            )],
        );
        let error = refusal(&root);
        assert!(
            error.contains("crates/example/src/lib.rs:2: `\"main\"` names neither"),
            "{error}"
        );
    }

    #[test]
    fn the_constants_and_an_allowlisted_use_pass() {
        let root = tree(
            "allowed",
            &[
                (
                    "crates/dim-agent-types/src/events.rs",
                    "pub const ROOT_BRANCH_ID: &str = \"main\";\npub const DEFAULT_BRANCH_ID: &str = ROOT_BRANCH_ID;\n",
                ),
                (
                    "web/src/rpc/protocol.ts",
                    "export const ROOT_BRANCH_ID = \"main\"\nexport const DEFAULT_BRANCH_ID = ROOT_BRANCH_ID\n",
                ),
                (
                    "crates/dim-app-server/src/storage/sqlite.rs",
                    "fn check(connection: &Connection) {\n    let _ = connection.is_readonly(\"main\");\n}\n",
                ),
            ],
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_definition_outside_its_home_is_refused_with_its_line() {
        let root = tree(
            "duplicate",
            &[(
                "crates/example/src/lib.rs",
                "fn root() -> &'static str { ROOT_BRANCH_ID }\nconst ROOT_BRANCH_ID: &str = \"main\";\n",
            )],
        );
        let error = refusal(&root);
        assert!(
            error.contains("crates/example/src/lib.rs:2: a branch id is defined once"),
            "{error}"
        );
    }

    #[test]
    fn a_test_module_and_a_test_file_are_skipped() {
        let root = tree(
            "test",
            &[
                (
                    "crates/example/src/lib.rs",
                    "fn root() -> &'static str { ROOT_BRANCH_ID }\n\
                     #[cfg(test)]\n\
                     mod tests {\n    const ID: &str = \"main\";\n}\n\
                     #[cfg(all(test, unix))]\n\
                     mod unix_tests {\n    const ID: &str = \"main\";\n}\n",
                ),
                (
                    "crates/example/src/agent/tests.rs",
                    "fn fixture() -> String { \"main\".into() }\n",
                ),
            ],
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_allowlisted_use_of_another_kind_passes_and_a_new_one_fails() {
        let root = tree(
            "kinds",
            &[
                (
                    "crates/dim-app-server/src/web/content.rs",
                    "fn f(cleaned: &str) -> usize {\n    let _ = extract_tag_inner(&cleaned, \"main\");\n    match (false, tag) {\n        (false, \"p\"\n            | \"main\" | \"blockquote\",\n        ) => 1,\n        _ => 0,\n    }\n}\n",
                ),
                (
                    "crates/example/src/lib.rs",
                    "fn other() -> &'static str { \"main\" }\n",
                ),
            ],
        );
        let error = refusal(&root);
        assert!(error.contains("crates/example/src/lib.rs:1"), "{error}");
        assert!(!error.contains("web/content.rs"), "{error}");
    }

    #[test]
    fn a_windows_spelled_path_matches_the_homes_and_the_allowlist() {
        let root = Path::new("/repo");
        let windows = |path: &str| root.join(path.replace('/', "\\"));
        assert_eq!(
            relative(root, &windows("crates/dim-agent-types/src/events.rs")),
            "crates/dim-agent-types/src/events.rs"
        );
        assert_eq!(
            findings(
                "crates/dim-agent-types/src/events.rs",
                "pub const ROOT_BRANCH_ID: &str = \"main\";\n",
                &mut Vec::new()
            )
            .unwrap(),
            0
        );
        assert_eq!(
            findings(
                "crates/dim-app-server/src/storage/sqlite.rs",
                "fn check(connection: &Connection) {\n    let _ = connection.is_readonly(\"main\");\n}\n",
                &mut Vec::new()
            )
            .unwrap(),
            1
        );
        let errors = rust_findings("fn branch() -> &'static str {\n    \"main\"\n}\n");
        assert!(
            errors[0].contains("crates/example/src/lib.rs:2: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_gated_statement_macro_is_not_code_and_an_ungated_one_is() {
        let errors = rust_findings(
            "fn production() {\n    #[cfg(test)]\n    assert_eq!(id(), \"main\");\n}\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let errors = rust_findings("fn production() {\n    assert_eq!(id(), \"main\");\n}\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("crates/example/src/lib.rs:2: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_gated_expression_statement_is_not_code() {
        let errors =
            rust_findings("fn production() {\n    #[cfg(test)]\n    { let _ = \"main\"; }\n}\n");
        assert!(errors.is_empty(), "{errors:?}");
        let errors = rust_findings("fn production() {\n    { let _ = \"main\"; }\n}\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn a_gated_match_arm_is_not_code_and_an_ungated_one_is() {
        let gated = "fn production(line: Option<u8>) -> &'static str {\n    match line {\n        #[cfg(test)]\n        Some(_) => \"main\",\n        None => \"other\",\n    }\n}\n";
        let errors = rust_findings(gated);
        assert!(errors.is_empty(), "{errors:?}");
        let ungated = "fn production(line: Option<u8>) -> &'static str {\n    match line {\n        Some(_) => \"main\",\n        None => \"other\",\n    }\n}\n";
        let errors = rust_findings(ungated);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("crates/example/src/lib.rs:3: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_gated_struct_literal_field_is_not_code() {
        let errors = rust_findings(
            "struct S { name: &'static str }\n\nfn production() -> S {\n    S {\n        #[cfg(test)]\n        name: \"main\",\n    }\n}\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_gated_field_variant_and_foreign_item_are_not_code() {
        let gated = "struct S {\n    #[cfg(test)]\n    name: [u8; \"main\".len()],\n}\n\nenum E {\n    #[cfg(test)]\n    A([u8; \"main\".len()]),\n}\n\nextern \"C\" {\n    #[cfg(test)]\n    fn ext(name: [u8; \"main\".len()]);\n}\n";
        let errors = rust_findings(gated);
        assert!(errors.is_empty(), "{errors:?}");
        let ungated = "struct S {\n    name: [u8; \"main\".len()],\n}\n";
        let errors = rust_findings(ungated);
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn a_gated_expression_is_not_code_in_any_position() {
        for (gated, ungated) in [
            (
                "fn production(other: &'static str) {\n    let _ids = [#[cfg(test)] \"main\", other];\n}\n",
                "fn production(other: &'static str) {\n    let _ids = [\"main\", other];\n}\n",
            ),
            (
                "fn production() {\n    let _tuple = (#[cfg(test)] \"main\", 1);\n}\n",
                "fn production() {\n    let _tuple = (\"main\", 1);\n}\n",
            ),
            (
                "fn takes(_: &'static str) {}\nfn production() {\n    takes(#[cfg(test)] \"main\");\n}\n",
                "fn takes(_: &'static str) {}\nfn production() {\n    takes(\"main\");\n}\n",
            ),
            (
                "struct S;\nimpl S { fn takes(&self, _: &'static str) {} }\nfn production() {\n    S.takes(#[cfg(test)] \"main\");\n}\n",
                "struct S;\nimpl S { fn takes(&self, _: &'static str) {} }\nfn production() {\n    S.takes(\"main\");\n}\n",
            ),
            (
                "struct T(#[cfg(test)] &'static str, u8);\nfn production() {\n    let _ = T(#[cfg(test)] \"main\", 2);\n}\n",
                "struct T(&'static str, u8);\nfn production() {\n    let _ = T(\"main\", 2);\n}\n",
            ),
            (
                "fn production() {\n    let _tail = { #[cfg(test)] \"main\" };\n}\n",
                "fn production() {\n    let _tail = { \"main\" };\n}\n",
            ),
            (
                "struct S { name: &'static str }\nfn production(s: S) {\n    if let S { #[cfg(test)] name: \"main\", .. } = s {}\n}\n",
                "struct S { name: &'static str }\nfn production(s: S) {\n    if let S { name: \"main\", .. } = s {}\n}\n",
            ),
        ] {
            let errors = rust_findings(gated);
            assert!(errors.is_empty(), "{gated}: {errors:?}");
            let errors = rust_findings(ungated);
            assert_eq!(errors.len(), 1, "{ungated}: {errors:?}");
        }
        let errors = rust_findings(
            "fn production(other: &'static str) {\n    let _ids = [#[cfg(not(test))] \"main\", other];\n}\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn every_node_the_table_gates_is_gated() {
        for (gated, ungated) in [
            (
                "#![cfg(test)]\nfn production() -> &'static str { \"main\" }\n",
                "#![allow(dead_code)]\nfn production() -> &'static str { \"main\" }\n",
            ),
            (
                "fn takes(#[cfg(test)] name: [u8; \"main\".len()]) { let _ = name; }\nfn production() { takes(); }\n",
                "fn takes(name: [u8; \"main\".len()]) { let _ = name; }\n",
            ),
            (
                "fn production() {\n    let f: fn(#[cfg(test)] [u8; \"main\".len()]) = |_: [u8; 3]| {};\n    let _ = f;\n}\n",
                "fn production() {\n    let f: fn([u8; \"main\".len()]) = |_: [u8; 3]| {};\n    let _ = f;\n}\n",
            ),
            (
                "struct S<#[cfg(test)] const N: usize = { \"main\".len() }>([u8; N]);\n",
                "struct S<const N: usize = { \"main\".len() }>([u8; N]);\n",
            ),
            (
                "fn production() {\n    let f = |#[cfg(test)] \"main\"| {};\n    f();\n}\n",
                "fn production() {\n    let f = |\"main\"| {};\n}\n",
            ),
        ] {
            let errors = rust_findings(gated);
            assert!(errors.is_empty(), "{gated}: {errors:?}");
            let errors = rust_findings(ungated);
            assert_eq!(errors.len(), 1, "{ungated}: {errors:?}");
        }
        let errors = rust_findings("unsafe extern \"C\" { fn f(#[cfg(test)] \"main\": ...); }\n");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_commented_out_script_is_markup_and_the_real_script_is_code() {
        let errors = vue_findings(
            "<!-- <script>const x = \"main\"</script> -->\n<script setup>\nconst id = ROOT_BRANCH_ID\n</script>\n<template><p>hello</p></template>\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let errors = vue_findings(
            "<!-- <script>const x = 1</script> -->\n<script setup>\nconst id = \"main\"\n</script>\n<template><p>hello</p></template>\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("LinesTree.vue:3: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_main_in_a_directive_expression_is_refused_with_its_line() {
        for (text, expected) in [
            (
                "<template>\n  <li v-if=\"line.id === 'main'\">root</li>\n</template>\n",
                1,
            ),
            (
                "<template>\n  <li :class=\"{ active: id === 'main' }\">x</li>\n</template>\n",
                1,
            ),
            (
                "<template>\n  <li @click=\"select('main')\">{{ t('x') }}</li>\n</template>\n",
                1,
            ),
        ] {
            let errors = vue_findings(text);
            assert_eq!(errors.len(), expected, "{text}: {errors:?}");
            assert!(
                errors[0]
                    .contains("web/src/features/chat/LinesTree.vue:2: `\"main\"` names neither"),
                "{errors:?}"
            );
        }
    }

    #[test]
    fn a_plain_attribute_is_not_code() {
        for text in [
            "<template>\n  <main id=\"main\" class=\"page\"></main>\n</template>\n",
            "<template>\n  <div title=\"main\"></div>\n</template>\n",
        ] {
            let errors = vue_findings(text);
            assert!(errors.is_empty(), "{text}: {errors:?}");
        }
        let errors = vue_findings(
            "<template>\n  <main :id=\"'main'\" class=\"page\"></main>\n</template>\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn an_interpolation_is_code_in_template_text_and_not_in_an_attribute() {
        let mut failures = Vec::new();
        for (text, uses) in [
            (
                "<template>\n  <div title=\"Example: {{ 'main' }}\">Help</div>\n</template>\n",
                0,
            ),
            ("<template>\n  <div>{{ 'main' }}</div>\n</template>\n", 1),
            (
                "<template>\n  <div title=\"a > {{ 'main' }}\">x</div>\n</template>\n",
                0,
            ),
            (
                "<template>\n  <div :title=\"'main'\">x</div>\n</template>\n",
                1,
            ),
        ] {
            let found = vue_findings(text).len();
            if found != uses {
                failures.push(format!("{text:?}: left {found}, right {uses}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Every fixture this module scans, including each round's reviewers'
    /// input.
    const FIXTURES: &[(&str, &str)] = &[
        (
            "crates/example/src/lib.rs",
            "fn branch() -> &'static str {\n    \"main\"\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "pub const ROOT_BRANCH_ID: &str = \"main\";\n",
        ),
        (
            "crates/example/src/lib.rs",
            "/// A `\"main\"` in a doc example.\nfn a() -> &'static str { \"main\" }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    const A: &str = \"}\";\n    const ID: &str = \"main\";\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {\n        let s = b\"\\\"\";\n        let id = \"main\";\n    }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "/// A gate is written `#[cfg(test)]` above a test module.\nfn production() -> &'static str { \"main\" }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn one() { let _ = json!({\"branchId\": \"main\"}); }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "#[cfg(test)]\nmod tests {\n    fn a() { let _ = json!({\"branchId\": \"main\"}); }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    #[cfg(test)]\n    assert_eq!(id(), \"main\");\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    #[cfg(test)]\n    { let _ = \"main\"; }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production(line: Option<u8>) -> &'static str {\n    match line {\n        #[cfg(test)]\n        Some(_) => \"main\",\n        None => \"other\",\n    }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S { name: &'static str }\nfn production() -> S {\n    S {\n        #[cfg(test)]\n        name: \"main\",\n    }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S {\n    #[cfg(test)]\n    name: [u8; \"main\".len()],\n}\n\nenum E {\n    #[cfg(test)]\n    A([u8; \"main\".len()]),\n}\n\nextern \"C\" {\n    #[cfg(test)]\n    fn ext(name: [u8; \"main\".len()]);\n}\n",
        ),
        (
            "web/src/a.vue",
            "<!-- <script>const x = \"main\"</script> -->\n<script setup>\nconst id = ROOT_BRANCH_ID\n</script>\n<template><p>hello</p></template>\n",
        ),
        ("web/src/a.vue", "<template>{{ 1 < 2 }}</template>"),
        ("web/src/a.vue", "<template>{{ a <b }}</template>"),
        ("web/src/a.vue", "<template>x <</template>"),
        ("web/src/a.vue", "<template>{{ 1<数量 }}</template>"),
        ("web/src/a.vue", "<template><p title=é>x</p><é</template>"),
        (
            "web/src/a.vue",
            "<template>\n  <!-- {{ id === 'main' }} -->\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <!-- <p :x='\"main\"'></p> -->\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <p>{{ id === 'main' }}</p>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <p :x='\"main\"'></p>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <p>say \"main\"</p>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<script setup>\nconst s = \"{{ x }}\"\n</script>\n<template><p>hello</p></template>\n",
        ),
        (
            "web/src/a.vue",
            "<script setup>\nconst id = 'main'\n// \"main\"\n</script>\n<template/>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <li @click=\"select('main')\">{{ t('x') }}</li>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <div title=\"Example: {{ 'main' }}\">Help</div>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <div>{{ 'main' }}</div>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <div title=\"a > {{ 'main' }}\">x</div>\n</template>\n",
        ),
        (
            "web/src/a.vue",
            "<template>\n  <div :title=\"'main'\">x</div>\n</template>\n",
        ),
        (
            "web/src/a.ts",
            "const id = \"main\"\n// \"main\"\n/* \"main\" */\n",
        ),
        (
            "web/src/a.ts",
            "const a = 'main', b = `main`, c = \"main\"\n",
        ),
        ("web/src/a.ts", "const a = \"unterminated\n"),
        ("web/src/a.ts", "const label = `${line ?? 'main'}`\n"),
        ("web/src/a.ts", "const label = `main is ${x}`\n"),
        (
            "web/src/a.ts",
            "const label = `a ${ `b ${ f('main') }` }`\n",
        ),
        ("web/src/a.ts", "const label = `a ${ `b` + 'main' }`\n"),
        (
            "web/src/a.ts",
            "const trimQuote = /'/g\nconst id = 'main'\n",
        ),
        ("web/src/a.ts", "const re = /[/']/\nconst id = 'main'\n"),
        ("web/src/a.ts", "const r = a / b; const id = 'main'\n"),
        ("web/src/a.ts", "return /'/.test(x) ? y : 'main'\n"),
        ("web/src/a.ts", "if (enabled) /\"main\"/.test(label);\n"),
        ("web/src/a.ts", "if (enabled)/\"main\"/.test(label);\n"),
        (
            "web/src/a.ts",
            "while (ready) /'/.test(x);\nconst a = 'main';\n",
        ),
        (
            "web/src/a.ts",
            "while (ready)/'/.test(x);\nconst a = 'main';\n",
        ),
        ("web/src/a.ts", "for (const x of xs) /\"main\"/.test(x);\n"),
        ("web/src/a.ts", "for (const x of xs)/\"main\"/.test(x);\n"),
        ("web/src/a.ts", "total(a) / 2; const b = \"main\";\n"),
        ("web/src/a.ts", "total(a) /\"main\"/ 2;\n"),
        ("web/src/a.ts", "function f() {}\n/\"main\"/.test(s);\n"),
        ("web/src/a.ts", "function f() {}/\"main\"/.test(s);\n"),
        ("web/src/a.ts", "if (x) { y(); } /\"main\"/.test(s);\n"),
        ("web/src/a.ts", "if (x) { y(); }/\"main\"/.test(s);\n"),
        ("web/src/a.ts", "const r = {} / 2; const b = \"main\";\n"),
        ("web/src/a.ts", "const r = {} /\"main\"/ 2;\n"),
        ("web/src/a.ts", "class A {}\n/\"main\"/.test(s);\n"),
        ("web/src/a.ts", "class A {}/\"main\"/.test(s);\n"),
        ("web/src/a.ts", "export default /\"main\"/.test(label);\n"),
        ("web/src/a.ts", "export default \"main\";\n"),
        (
            "web/src/a.ts",
            "const r = obj.default / 2; const b = \"main\";\n",
        ),
        (
            "web/src/a.ts",
            "export default { a: 1 } / 2; const b = \"main\";\n",
        ),
        (
            "web/src/a.ts",
            "export default function f() {}\n/\"main\"/.test(s);\n",
        ),
        ("web/src/a.ts", "class A extends /\"main\"/ {}\n"),
        (
            "web/src/a.ts",
            "class A extends base {}\nconst b = \"main\";\n",
        ),
        (
            "web/src/chat-ui/WebChatTimeline.vue",
            "<template>\n  <div :data-variant=\"variant\" @click=\"openLine('main')\"></div>\n</template>\n",
        ),
        (
            "crates/dim-app-server/src/storage/sqlite.rs",
            "fn check(connection: &Connection) {\n    if let Ok(false) = connection.is_readonly(\"main\") { let b = \"main\"; }\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production(other: &'static str) {\n    let _ids = [#[cfg(test)] \"main\", other];\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production(other: &'static str) {\n    let _ids = [\"main\", other];\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let _tuple = (#[cfg(test)] \"main\", 1);\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let _tuple = (\"main\", 1);\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn takes(_: &'static str) {}\nfn production() {\n    takes(#[cfg(test)] \"main\");\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn takes(_: &'static str) {}\nfn production() {\n    takes(\"main\");\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S;\nimpl S { fn takes(&self, _: &'static str) {} }\nfn production() {\n    S.takes(#[cfg(test)] \"main\");\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S;\nimpl S { fn takes(&self, _: &'static str) {} }\nfn production() {\n    S.takes(\"main\");\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct T(#[cfg(test)] &'static str, u8);\nfn production() {\n    let _ = T(#[cfg(test)] \"main\", 2);\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct T(&'static str, u8);\nfn production() {\n    let _ = T(\"main\", 2);\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let _tail = { #[cfg(test)] \"main\" };\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let _tail = { \"main\" };\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S { name: &'static str }\nfn production(s: S) {\n    if let S { #[cfg(test)] name: \"main\", .. } = s {}\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S { name: &'static str }\nfn production(s: S) {\n    if let S { name: \"main\", .. } = s {}\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production(other: &'static str) {\n    let _ids = [#[cfg(not(test))] \"main\", other];\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "#![cfg(test)]\nfn production() -> &'static str { \"main\" }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "#![allow(dead_code)]\nfn production() -> &'static str { \"main\" }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn takes(#[cfg(test)] name: [u8; \"main\".len()]) { let _ = name; }\nfn production() { takes(); }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn takes(name: [u8; \"main\".len()]) { let _ = name; }\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let f: fn(#[cfg(test)] [u8; \"main\".len()]) = |_: [u8; 3]| {};\n    let _ = f;\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let f: fn([u8; \"main\".len()]) = |_: [u8; 3]| {};\n    let _ = f;\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S<#[cfg(test)] const N: usize = { \"main\".len() }>([u8; N]);\n",
        ),
        (
            "crates/example/src/lib.rs",
            "struct S<const N: usize = { \"main\".len() }>([u8; N]);\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let f = |#[cfg(test)] \"main\"| {};\n    f();\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "fn production() {\n    let f = |\"main\"| {};\n}\n",
        ),
        (
            "crates/example/src/lib.rs",
            "unsafe extern \"C\" { fn f(#[cfg(test)] \"main\": ...); }\n",
        ),
    ];

    #[test]
    fn every_fixture_scans_without_panicking_or_hanging() {
        for (file, fixture) in FIXTURES {
            let _ = bounded(file, fixture);
        }
    }

    #[test]
    fn every_character_boundary_prefix_scans_without_panicking_or_hanging() {
        for (file, fixture) in FIXTURES {
            for end in (0..=fixture.len()).filter(|end| fixture.is_char_boundary(*end)) {
                let _ = bounded(file, &fixture[..end]);
            }
        }
    }

    #[test]
    fn zerob13s_byte_string_module_scans_and_a_later_production_use_is_refused() {
        let text = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a() {\n        let s = b\"\\\"\";\n        let id = \"main\";\n    }\n}\nfn after() -> &'static str { \"main\" }\n";
        let errors = rust_findings(text);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("crates/example/src/lib.rs:9: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_doc_line_that_mentions_a_gate_gates_nothing() {
        let text = "/// A gate is written `#[cfg(test)]` above a test module.\nfn production() -> &'static str { \"main\" }\n";
        let errors = rust_findings(text);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("crates/example/src/lib.rs:2: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn an_html_comment_holds_no_expression_and_the_same_code_outside_it_does() {
        for text in [
            "<template>\n  <!-- {{ id === \"main\" }} -->\n</template>\n",
            "<template>\n  <!-- <p :x='\"main\"'></p> -->\n</template>\n",
        ] {
            let errors = vue_findings(text);
            assert!(errors.is_empty(), "{text}: {errors:?}");
        }
        let errors = vue_findings("<template>\n  <p :x='\"main\"'></p>\n</template>\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        let errors = vue_findings("<template>\n  <p>{{ id === \"main\" }}</p>\n</template>\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn template_text_is_not_code() {
        let errors = vue_findings("<template>\n  <p>say \"main\"</p>\n</template>\n");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_macro_literal_is_a_use_and_a_gated_one_is_not() {
        let errors = rust_findings("fn one() { let _ = json!({\"branchId\": \"main\"}); }\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        let errors = rust_findings(
            "#[cfg(test)]\nmod tests {\n    fn a() { let _ = json!({\"branchId\": \"main\"}); }\n}\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_cfg_that_also_holds_without_test_is_production() {
        let root = tree(
            "cfg",
            &[(
                "crates/example/src/lib.rs",
                "#[cfg(any(test, unix))]\nfn a() -> &'static str { \"main\" }\n",
            )],
        );
        let error = refusal(&root);
        assert!(error.contains("crates/example/src/lib.rs:2"), "{error}");
        let root = tree(
            "cfg-not",
            &[(
                "crates/example/src/lib.rs",
                "#[cfg(not(test))]\nfn a() -> &'static str { \"main\" }\n",
            )],
        );
        let error = refusal(&root);
        assert!(error.contains("crates/example/src/lib.rs:2"), "{error}");
    }

    #[test]
    fn a_main_that_only_looks_like_one_is_not_a_literal() {
        let root = tree(
            "lexical",
            &[(
                "crates/example/src/lib.rs",
                "/// `session.service::<Timeline>(\"main\")` is a doc example.\n\
                 // a comment: \"main\"\n\
                 fn sample() -> &'static str { r#\"<dim-branch-line parent=\"main\">\"# }\n",
            )],
        );
        check(&root).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_script_block_is_code_and_a_template_interpolation_is_not_a_script() {
        let errors = vue_findings(
            "<script setup lang=\"ts\">\nconst id = 'main'\n</script>\n<template><p>hello</p></template>\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        let errors = vue_findings(
            "<script setup lang=\"ts\">\nconst s = \"{{ x }}\"\n</script>\n<template><p>hello</p></template>\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_typescript_comment_is_not_code_and_a_literal_is() {
        let errors = bounded("web/src/a.ts", "const id = \"main\"\n// \"main\"\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        let errors = bounded("web/src/a.ts", "// \"main\"\n/* \"main\" */\n");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_template_interpolation_is_code_and_its_text_is_not() {
        let errors = bounded("web/src/a.ts", "const label = `${line ?? 'main'}`\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("web/src/a.ts:1: `\"main\"` names neither"),
            "{errors:?}"
        );
        let errors = bounded("web/src/a.ts", "const label = `main is ${x}`\n");
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn a_template_whose_whole_text_is_the_literal_is_one() {
        let errors = bounded("web/src/a.ts", "const id = `main`\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn a_nested_template_interpolation_is_code() {
        let errors = bounded(
            "web/src/a.ts",
            "const label = `a ${ `b ${ f('main') }` }`\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("web/src/a.ts:1: `\"main\"` names neither"),
            "{errors:?}"
        );
        let errors = bounded("web/src/a.ts", "const label = `a ${ `b` + 'main' }`\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn a_quote_in_a_regex_does_not_open_a_string() {
        let errors = bounded(
            "web/src/a.ts",
            "const trimQuote = /'/g\nconst id = 'main'\n",
        );
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("web/src/a.ts:2: `\"main\"` names neither"),
            "{errors:?}"
        );
        let errors = bounded("web/src/a.ts", "const re = /[/']/\nconst id = 'main'\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("web/src/a.ts:2: `\"main\"` names neither"),
            "{errors:?}"
        );
    }

    #[test]
    fn a_slash_after_a_control_flow_header_or_a_block_opens_a_regex() {
        let mut failures = Vec::new();
        for (text, uses) in [
            ("if (enabled) /\"main\"/.test(label);\n", 0),
            ("if (enabled)/\"main\"/.test(label);\n", 0),
            ("while (ready) /'/.test(x);\nconst a = 'main';\n", 1),
            ("while (ready)/'/.test(x);\nconst a = 'main';\n", 1),
            ("for (const x of xs) /\"main\"/.test(x);\n", 0),
            ("for (const x of xs)/\"main\"/.test(x);\n", 0),
            ("total(a) / 2; const b = \"main\";\n", 1),
            ("total(a) /\"main\"/ 2;\n", 1),
            ("function f() {}\n/\"main\"/.test(s);\n", 0),
            ("function f() {}/\"main\"/.test(s);\n", 0),
            ("if (x) { y(); } /\"main\"/.test(s);\n", 0),
            ("if (x) { y(); }/\"main\"/.test(s);\n", 0),
            ("const r = {} / 2; const b = \"main\";\n", 1),
            ("const r = {} /\"main\"/ 2;\n", 1),
            ("class A {}\n/\"main\"/.test(s);\n", 0),
            ("class A {}/\"main\"/.test(s);\n", 0),
        ] {
            let found = bounded("web/src/a.ts", text).len();
            if found != uses {
                failures.push(format!("{text:?}: left {found}, right {uses}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_slash_after_a_keyword_with_an_expression_operand_opens_a_regex() {
        let mut failures = Vec::new();
        for (text, uses) in [
            ("export default /\"main\"/.test(label);\n", 0),
            ("export default \"main\";\n", 1),
            ("const r = obj.default / 2; const b = \"main\";\n", 1),
            ("export default { a: 1 } / 2; const b = \"main\";\n", 1),
            ("export default function f() {}\n/\"main\"/.test(s);\n", 0),
            ("class A extends /\"main\"/ {}\n", 0),
            ("class A extends base {}\nconst b = \"main\";\n", 1),
        ] {
            let found = bounded("web/src/a.ts", text).len();
            if found != uses {
                failures.push(format!("{text:?}: left {found}, right {uses}"));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn a_slash_that_divides_is_not_a_regex_literal() {
        let errors = bounded("web/src/a.ts", "const r = a / b; const id = 'main'\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        let errors = bounded("web/src/a.ts", "return /'/.test(x) ? y : 'main'\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
    }

    #[test]
    fn an_allowlist_entry_covers_only_the_literal_it_spans() {
        let codoggys = "<template>\n  <div :data-variant=\"variant\" @click=\"openLine('main')\"></div>\n</template>\n";
        let errors = bounded("web/src/chat-ui/WebChatTimeline.vue", codoggys);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("WebChatTimeline.vue:2: `\"main\"` names neither"),
            "{errors:?}"
        );
        let real = "<template>\n  <div :data-variant=\"variant ?? 'main'\"></div>\n</template>\n";
        let errors = bounded("web/src/chat-ui/WebChatTimeline.vue", real);
        assert!(errors.is_empty(), "{errors:?}");
        let both = "fn check(connection: &Connection) {\n    if let Ok(false) = connection.is_readonly(\"main\") { let b = \"main\"; }\n}\n";
        let errors = bounded("crates/dim-app-server/src/storage/sqlite.rs", both);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("storage/sqlite.rs:2: `\"main\"` names neither"),
            "{errors:?}"
        );
    }
}
