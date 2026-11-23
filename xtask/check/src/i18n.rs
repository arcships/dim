//! `check` fails when a locale table does not hold the Simplified Chinese
//! table's keys, or when a translation call (`t`, `tf`, `t_in`, `tf_in`) or a
//! `ListPanel` text passes a key the Simplified Chinese table lacks. A key is
//! the English text itself (`crates/dim-cli/src/ui/i18n/mod.rs`), so a table is
//! a `lookup` function whose match arms are string literals, and a call site is
//! a key argument that reduces to string literals: one literal, the text of a
//! constant in scope, the branches of an `if`, `match` or block, or the
//! expressions a macro argument separates by commas. `ListPanel` keeps its
//! title, loading text, empty text and no-match text English until it draws, so
//! those arguments are keys too.
//!
//! Both sides are parsed, not matched by hand: a table arm whose pattern is
//! neither a string nor the catch-all is a failure, so a new shape cannot pass
//! by being skipped. A `#[cfg(test)]` item is a fixture rather than text the
//! interface draws, so the scan skips it.
//!
//! A constant is read only where its name still means it: a parameter, a local,
//! a pattern, a `use` import, a static, a glob import or a constant an inner
//! scope declares hides an outer one, including one a block a key argument
//! evaluates to declares, and a name no lookup settles is counted
//! instead. An inline module and a macro in statement or item position may hold
//! a name the scan cannot see, so neither reads an outer constant either. A key
//! argument that reduces to no literal is counted for the same reason: it names
//! a parameter, a field, the result of a call or a constant of another file.
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use syn::{
    Attribute, Block, Expr, ExprLit, ImplItem, Item, Lit, Meta, Pat, Stmt, Token,
    parse::Parser,
    punctuated::Punctuated,
    spanned::Spanned,
    visit::{self, Visit},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// The locale tables, one directory per locale, one file per area.
const TABLES: &str = "crates/dim-cli/src/ui/i18n";
/// The sources whose translation calls are checked.
const SOURCE: &str = "crates/dim-cli/src";
/// The table every locale must hold the keys of.
const BASE: &str = "zh_cn";
/// The `ListPanel::new` arguments that hold text the panel translates when it
/// draws: the title, the loading text, the empty text and the no-match text.
const LIST_PANEL_TEXTS: &[usize] = &[0, 2, 3, 4];

/// Checks every locale table and every literal call key. Each failure names
/// the table or the call site and the key.
pub fn check(root: &Path) -> Result<()> {
    let tables = tables(root)?;
    let base = tables
        .get(BASE)
        .ok_or_else(|| format!("{TABLES}/{BASE}: no table"))?;
    let mut errors = Vec::new();
    for (locale, keys) in &tables {
        if locale == BASE {
            continue;
        }
        for key in base.difference(keys) {
            errors.push(format!("{TABLES}/{locale}: lacks {key:?}"));
        }
        for key in keys.difference(base) {
            errors.push(format!("{TABLES}/{locale}: {key:?} is not in {BASE}"));
        }
    }
    let calls = calls(root)?;
    for (key, site) in &calls.keys {
        if !base.contains(key) {
            errors.push(format!("{site}: {key:?} is not in {BASE}"));
        }
    }
    if errors.is_empty() {
        println!(
            "i18n keys passed ({} tables, {} literal keys, {} key arguments without a literal)",
            tables.len(),
            calls.keys.len(),
            calls.unreadable.len()
        );
        Ok(())
    } else {
        Err(errors.join("\n").into())
    }
}

/// Every locale's keys, read from its `lookup` arms.
fn tables(root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut tables = BTreeMap::new();
    for entry in fs::read_dir(root.join(TABLES))? {
        let path = entry?.path();
        if !path.is_dir() {
            continue;
        }
        let mut keys = BTreeSet::new();
        for area in sources(&path) {
            if path_name(&area) == "mod.rs" {
                continue;
            }
            let name = area.display().to_string();
            keys.extend(table_keys(&fs::read_to_string(&area)?, &name)?);
        }
        tables.insert(path_name(&path), keys);
    }
    Ok(tables)
}

/// The keys of one area table (`text`, named `name` in failures): the string
/// literals its `lookup` match arms test, with the catch-all arm skipped.
fn table_keys(text: &str, name: &str) -> Result<BTreeSet<String>> {
    let file: syn::File = syn::parse_file(text)?;
    let function = file
        .items
        .into_iter()
        .find_map(|item| match item {
            Item::Fn(function) if function.sig.ident == "lookup" => Some(function),
            _ => None,
        })
        .ok_or_else(|| format!("{name}: no lookup function"))?;
    let Some(Stmt::Expr(Expr::Call(call), None)) = function.block.stmts.last() else {
        return Err(format!("{name}: lookup does not return Some(match ..)").into());
    };
    let Some(Expr::Match(expression)) = call.args.first() else {
        return Err(format!("{name}: lookup does not return Some(match ..)").into());
    };
    let mut keys = BTreeSet::new();
    for arm in &expression.arms {
        match &arm.pat {
            Pat::Lit(literal) => match &literal.lit {
                Lit::Str(key) => {
                    keys.insert(key.value());
                }
                _ => return Err(format!("{name}: an arm's key is not a string").into()),
            },
            Pat::Wild(_) => {}
            _ => {
                return Err(format!(
                    "{name}: an arm's pattern is neither a string nor the catch-all"
                )
                .into());
            }
        }
    }
    Ok(keys)
}

/// Every key the sources pass to a translation call or to a `ListPanel` text,
/// and every key argument that reduces to no literal.
fn calls(root: &Path) -> Result<CallKeys> {
    let tables = root.join(TABLES);
    let mut found = CallKeys::default();
    for path in sources(&root.join(SOURCE)) {
        if path.starts_with(&tables) {
            continue;
        }
        let site = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        found.extend(call_keys(&fs::read_to_string(&path)?, &site)?);
    }
    Ok(found)
}

/// What the calls in one source pass: each key with the site that passes it,
/// and the site of each key argument the check cannot read.
#[derive(Default)]
struct CallKeys {
    keys: Vec<(String, String)>,
    unreadable: Vec<String>,
}

impl CallKeys {
    fn extend(&mut self, other: Self) {
        for key in other.keys {
            if !self.keys.contains(&key) {
                self.keys.push(key);
            }
        }
        self.unreadable.extend(other.unreadable);
    }
}

/// Every key in `text` (`site` names it in the result), with the line it is
/// passed on.
fn call_keys(text: &str, site: &str) -> Result<CallKeys> {
    let file: syn::File = syn::parse_file(text)?;
    let mut calls = Calls {
        site: site.to_owned(),
        scopes: Vec::new(),
        counted: BTreeSet::new(),
        found: CallKeys::default(),
    };
    calls.visit_file(&file);
    Ok(calls.found)
}

/// Whether `attributes` holds `#[cfg(test)]`, so the item is a fixture rather
/// than text the interface draws.
fn test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args_with(Punctuated::<Meta, Token![,]>::parse_terminated)
                .is_ok_and(|predicates| {
                    predicates
                        .iter()
                        .any(|predicate| predicate.path().is_ident("test"))
                })
    })
}

/// Whether `item` is a fixture.
fn test_item(item: &Item) -> bool {
    match item {
        Item::Const(item) => test_only(&item.attrs),
        Item::Fn(item) => test_only(&item.attrs),
        Item::Impl(item) => test_only(&item.attrs),
        Item::Macro(item) => test_only(&item.attrs),
        Item::Mod(item) => test_only(&item.attrs),
        Item::Static(item) => test_only(&item.attrs),
        Item::Trait(item) => test_only(&item.attrs),
        _ => false,
    }
}

/// Whether `item` is a fixture.
fn test_impl_item(item: &ImplItem) -> bool {
    match item {
        ImplItem::Const(item) => test_only(&item.attrs),
        ImplItem::Fn(item) => test_only(&item.attrs),
        _ => false,
    }
}

/// The argument positions of a call that hold a translation key, when its name
/// is one the check reads.
fn key_positions(function: &Expr) -> Option<&'static [usize]> {
    let Expr::Path(function) = function else {
        return None;
    };
    let mut segments = function.path.segments.iter().rev();
    match segments.next()?.ident.to_string().as_str() {
        "t" | "tf" => Some(&[0]),
        "t_in" | "tf_in" => Some(&[1]),
        "new"
            if segments
                .next()
                .is_some_and(|module| module.ident == "ListPanel") =>
        {
            Some(LIST_PANEL_TEXTS)
        }
        _ => None,
    }
}

/// The text a `&str` constant holds, when its value is one string.
fn constant_value(ty: &syn::Type, value: &Expr) -> Option<String> {
    let syn::Type::Reference(reference) = ty else {
        return None;
    };
    let syn::Type::Path(text) = &*reference.elem else {
        return None;
    };
    if !text.path.is_ident("str") {
        return None;
    }
    match value {
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => Some(text.value()),
        _ => None,
    }
}

/// The names one scope declares, with the text of each constant, and whether a
/// lookup may read a name outside it.
#[derive(Default)]
struct Scope {
    names: BTreeMap<String, Option<String>>,
    /// A glob import, an inline module or a macro may bring in a name the scan
    /// cannot see, so no lookup leaves the scope.
    barrier: bool,
}

impl Scope {
    /// Declares `pattern`'s names as ones no scan can read. A pattern macro
    /// may bind a name the scan cannot see, including one declared here.
    fn bind(&mut self, pattern: &Pat) {
        let (names, macro_pattern) = bindings(pattern);
        if macro_pattern {
            self.names.clear();
            self.barrier = true;
            return;
        }
        for name in names {
            self.names.insert(name, None);
        }
    }
}

/// The tokens of every macro `pattern` holds.
#[derive(Default)]
struct PatternMacros(Vec<TokenStream>);

impl<'ast> Visit<'ast> for PatternMacros {
    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.0.push(node.mac.tokens.clone());
    }
}

/// The names `pattern` binds, and whether it holds a pattern macro that may
/// bind more.
fn bindings(pattern: &Pat) -> (BTreeSet<String>, bool) {
    struct Bindings {
        names: BTreeSet<String>,
        macro_pattern: bool,
    }
    impl<'ast> Visit<'ast> for Bindings {
        fn visit_pat_ident(&mut self, node: &'ast syn::PatIdent) {
            self.names.insert(node.ident.to_string());
            visit::visit_pat_ident(self, node);
        }
        fn visit_expr_macro(&mut self, _node: &'ast syn::ExprMacro) {
            self.macro_pattern = true;
        }
    }
    let mut bindings = Bindings {
        names: BTreeSet::new(),
        macro_pattern: false,
    };
    bindings.visit_pat(pattern);
    (bindings.names, bindings.macro_pattern)
}

/// The names `items` declare, for the scope they open: an item is in scope
/// whole, so a call before the declaration reads it too.
fn scope<'a>(items: impl IntoIterator<Item = &'a Item>) -> Scope {
    let mut scope = Scope::default();
    for item in items {
        if test_item(item) {
            continue;
        }
        match item {
            Item::Const(item) => {
                scope
                    .names
                    .insert(item.ident.to_string(), constant_value(&item.ty, &item.expr));
            }
            Item::Static(item) => {
                scope.names.insert(item.ident.to_string(), None);
            }
            Item::Use(item) => use_tree(&item.tree, None, &mut scope),
            Item::ForeignMod(item) => {
                for item in &item.items {
                    if let syn::ForeignItem::Static(item) = item {
                        scope.names.insert(item.ident.to_string(), None);
                    }
                }
            }
            Item::Macro(item) if !item.mac.path.is_ident("macro_rules") => scope.barrier = true,
            _ => {}
        }
    }
    scope
}

/// Declares the names `tree` binds: a `use` import hides an outer constant of
/// the same name, and a glob import hides every outer name.
fn use_tree(tree: &syn::UseTree, parent: Option<&syn::Ident>, scope: &mut Scope) {
    match tree {
        syn::UseTree::Path(path) => use_tree(&path.tree, Some(&path.ident), scope),
        syn::UseTree::Name(name) => {
            let bound = if name.ident == "self" {
                parent
            } else {
                Some(&name.ident)
            };
            if let Some(name) = bound {
                scope.names.insert(name.to_string(), None);
            }
        }
        syn::UseTree::Rename(rename) => {
            scope.names.insert(rename.rename.to_string(), None);
        }
        syn::UseTree::Group(group) => {
            for tree in &group.items {
                use_tree(tree, parent, scope);
            }
        }
        syn::UseTree::Glob(_) => scope.barrier = true,
    }
}

/// The scope a signature opens: its parameters hide a constant of the same
/// name.
fn parameters(signature: &syn::Signature) -> Scope {
    let mut scope = Scope::default();
    for input in &signature.inputs {
        if let syn::FnArg::Typed(typed) = input {
            scope.bind(&typed.pat);
        }
    }
    scope
}

/// Whether `tokens` holds a call to a translation function that is named
/// directly rather than built from an expression.
fn mentions_translation_call(tokens: &TokenStream) -> bool {
    let mut tokens = tokens.clone().into_iter().peekable();
    while let Some(token) = tokens.next() {
        match token {
            TokenTree::Ident(name)
                if matches!(name.to_string().as_str(), "t" | "tf" | "t_in" | "tf_in")
                    && matches!(
                        tokens.peek(),
                        Some(TokenTree::Group(group))
                            if group.delimiter() == Delimiter::Parenthesis
                    ) =>
            {
                return true;
            }
            TokenTree::Group(group) if mentions_translation_call(&group.stream()) => return true,
            _ => {}
        }
    }
    false
}

struct Calls {
    site: String,
    /// The scopes the call sits in, innermost last, each with the names it
    /// declares and whether a lookup stops there.
    scopes: Vec<Scope>,
    /// The arguments already counted, so one reached twice counts once.
    counted: BTreeSet<(usize, usize)>,
    found: CallKeys,
}

impl Calls {
    /// Adds every literal `expr` reduces to, looking through the branches of an
    /// `if`, `match` or block, a parenthesized expression and a nested
    /// translation call.
    fn literals(&mut self, expr: &Expr) {
        match expr {
            Expr::Lit(ExprLit {
                lit: Lit::Str(key), ..
            }) => self.key(&key.value(), key.span()),
            Expr::Path(node) => match self.constant(&node.path) {
                Some(text) => self.key(&text, expr.span()),
                None => self.unreadable(expr.span()),
            },
            Expr::If(node) => {
                self.scopes.push(Scope::default());
                self.visit_expr(&node.cond);
                self.block(&node.then_branch);
                self.scopes.pop();
                if let Some((_, otherwise)) = &node.else_branch {
                    self.literals(otherwise);
                }
            }
            Expr::Match(node) => {
                for arm in &node.arms {
                    self.scopes.push(Scope::default());
                    self.bind(&arm.pat);
                    if let Some((_, guard)) = &arm.guard {
                        self.scopes.push(Scope::default());
                        self.visit_expr(guard);
                        self.scopes.pop();
                    }
                    self.literals(&arm.body);
                    self.scopes.pop();
                }
            }
            Expr::Block(node) => self.block(&node.block),
            Expr::Paren(node) => self.literals(&node.expr),
            Expr::Group(node) => self.literals(&node.expr),
            Expr::Reference(node) => self.literals(&node.expr),
            Expr::Return(node) => {
                if let Some(value) = &node.expr {
                    self.literals(value);
                }
            }
            Expr::Call(node) => match key_positions(&node.func) {
                Some(positions) => {
                    for position in positions {
                        if let Some(argument) = node.args.get(*position) {
                            self.literals(argument);
                        }
                    }
                }
                None => self.unreadable(expr.span()),
            },
            _ => self.unreadable(expr.span()),
        }
    }

    /// Adds the literal a block evaluates to, read in the scope the block's own
    /// items and bindings open.
    fn block(&mut self, block: &Block) {
        self.statements(block, |calls| match block.stmts.last() {
            Some(Stmt::Expr(value, None)) => calls.literals(value),
            _ => calls.unreadable(block.brace_token.span.open()),
        });
    }

    /// Visits `block`'s statements in the scope its items and bindings open,
    /// then runs `visit` in that same scope.
    fn statements(&mut self, block: &Block, visit: impl FnOnce(&mut Self)) {
        let scope = scope(block.stmts.iter().filter_map(|statement| match statement {
            Stmt::Item(item) => Some(item),
            _ => None,
        }));
        self.scoped(scope, |calls| {
            for statement in &block.stmts {
                calls.visit_stmt(statement);
                if let Stmt::Local(local) = statement {
                    calls.bind(&local.pat);
                }
            }
            visit(calls);
        });
    }

    /// Adds `text` at `span`, unless it is empty: a panel draws nothing for
    /// one, so no table holds it.
    fn key(&mut self, text: &str, span: proc_macro2::Span) {
        if text.is_empty() {
            return;
        }
        let found = (
            text.to_owned(),
            format!("{}:{}", self.site, span.start().line),
        );
        if !self.found.keys.contains(&found) {
            self.found.keys.push(found);
        }
    }

    /// The text of the constant the bare path names, when the innermost scope
    /// that declares the name declares a `&str` constant.
    fn constant(&self, path: &syn::Path) -> Option<String> {
        let name = path.get_ident()?.to_string();
        for scope in self.scopes.iter().rev() {
            if let Some(value) = scope.names.get(&name) {
                return value.clone();
            }
            if scope.barrier {
                return None;
            }
        }
        None
    }

    /// Counts `span` as one key argument the check cannot read.
    fn unreadable(&mut self, span: proc_macro2::Span) {
        let start = span.start();
        if !self.counted.insert((start.line, start.column)) {
            return;
        }
        self.found
            .unreadable
            .push(format!("{}:{}", self.site, start.line));
    }

    /// Declares the names `pattern` introduces in the innermost scope, and
    /// reads a translation call a pattern macro holds.
    fn bind(&mut self, pattern: &Pat) {
        let mut macros = PatternMacros::default();
        macros.visit_pat(pattern);
        for tokens in macros.0 {
            self.macro_tokens(&tokens);
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.bind(pattern);
        }
    }

    /// Marks the innermost scope as one that may hold a name no scan can see,
    /// including one it declares itself.
    fn pollute(&mut self) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.clear();
            scope.barrier = true;
        }
    }

    /// Runs `visit` inside a scope whose declared names hide an outer one's.
    fn scoped(&mut self, scope: Scope, visit: impl FnOnce(&mut Self)) {
        self.scopes.push(scope);
        visit(self);
        self.scopes.pop();
    }

    /// Reads a macro's tokens as the expressions its commas separate, and
    /// counts the macro as one argument the scan cannot read when they do not
    /// parse but hold a translation call.
    fn macro_tokens(&mut self, tokens: &TokenStream) {
        match Punctuated::<Expr, Token![,]>::parse_terminated.parse2(tokens.clone()) {
            Ok(expressions) => {
                for expression in &expressions {
                    self.visit_expr(expression);
                }
            }
            Err(_) if mentions_translation_call(tokens) => self.unreadable(tokens.span()),
            Err(_) => {}
        }
    }
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_file(&mut self, node: &'ast syn::File) {
        self.scoped(scope(&node.items), |calls| visit::visit_file(calls, node));
    }

    fn visit_item(&mut self, node: &'ast Item) {
        if test_item(node) {
            return;
        }
        match node {
            Item::Mod(item) if item.content.is_some() => {
                let mut scope = scope(item.content.iter().flat_map(|(_, items)| items.iter()));
                scope.barrier = true;
                self.scoped(scope, |calls| visit::visit_item(calls, node));
            }
            Item::Macro(item) => {
                self.macro_tokens(&item.mac.tokens);
                if !item.mac.path.is_ident("macro_rules") {
                    self.pollute();
                }
                visit::visit_item(self, node);
            }
            _ => visit::visit_item(self, node),
        }
    }

    fn visit_impl_item(&mut self, node: &'ast ImplItem) {
        if !test_impl_item(node) {
            visit::visit_impl_item(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.scoped(parameters(&node.sig), |calls| {
            visit::visit_item_fn(calls, node)
        });
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        self.scoped(parameters(&node.sig), |calls| {
            visit::visit_impl_item_fn(calls, node)
        });
    }

    fn visit_trait_item_fn(&mut self, node: &'ast syn::TraitItemFn) {
        self.scoped(parameters(&node.sig), |calls| {
            visit::visit_trait_item_fn(calls, node)
        });
    }

    fn visit_block(&mut self, node: &'ast Block) {
        self.statements(node, |_| {});
    }

    fn visit_expr_closure(&mut self, node: &'ast syn::ExprClosure) {
        self.scopes.push(Scope::default());
        for input in &node.inputs {
            self.bind(input);
        }
        visit::visit_expr_closure(self, node);
        self.scopes.pop();
    }

    fn visit_expr_for_loop(&mut self, node: &'ast syn::ExprForLoop) {
        self.scopes.push(Scope::default());
        self.visit_expr(&node.expr);
        self.bind(&node.pat);
        self.visit_block(&node.body);
        self.scopes.pop();
    }

    fn visit_expr_if(&mut self, node: &'ast syn::ExprIf) {
        self.scopes.push(Scope::default());
        self.visit_expr(&node.cond);
        self.visit_block(&node.then_branch);
        self.scopes.pop();
        if let Some((_, otherwise)) = &node.else_branch {
            self.visit_expr(otherwise);
        }
    }

    fn visit_expr_while(&mut self, node: &'ast syn::ExprWhile) {
        self.scopes.push(Scope::default());
        self.visit_expr(&node.cond);
        self.visit_block(&node.body);
        self.scopes.pop();
    }

    fn visit_expr_let(&mut self, node: &'ast syn::ExprLet) {
        self.visit_expr(&node.expr);
        self.bind(&node.pat);
    }

    fn visit_arm(&mut self, node: &'ast syn::Arm) {
        self.scopes.push(Scope::default());
        self.bind(&node.pat);
        if let Some((_, guard)) = &node.guard {
            self.scopes.push(Scope::default());
            self.visit_expr(guard);
            self.scopes.pop();
        }
        self.visit_expr(&node.body);
        self.scopes.pop();
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let Some(positions) = key_positions(&node.func) {
            for position in positions {
                if let Some(argument) = node.args.get(*position) {
                    self.literals(argument);
                }
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        if node.method == "set_empty_text"
            && let Some(argument) = node.args.first()
        {
            self.literals(argument);
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_expr_macro(&mut self, node: &'ast syn::ExprMacro) {
        self.macro_tokens(&node.mac.tokens);
        visit::visit_expr_macro(self, node);
    }

    fn visit_stmt_macro(&mut self, node: &'ast syn::StmtMacro) {
        self.macro_tokens(&node.mac.tokens);
        self.pollute();
        visit::visit_stmt_macro(self, node);
    }
}

/// The Rust sources under `directory`, recursively, sorted.
fn sources(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    sources_in(directory, &mut files);
    files.sort();
    files
}

fn sources_in(directory: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(directory).expect("a source directory reads") {
        let path = entry.expect("a source entry reads").path();
        if path.is_dir() {
            sources_in(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn path_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Result<BTreeSet<String>> {
        table_keys(text, "table.rs")
    }

    #[test]
    fn a_table_reads_every_literal_arm_and_skips_the_catch_all() {
        let keys = table(
            r#"pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Queue" => "Warteschlange",
        "a \"quoted\" key" => {
            "Wert"
        }
        _ => return None,
    })
}"#,
        )
        .unwrap();
        assert_eq!(
            keys,
            BTreeSet::from(["Queue".to_owned(), "a \"quoted\" key".to_owned()])
        );
    }

    #[test]
    fn an_arm_that_is_not_a_string_is_a_failure() {
        let error = table(
            r#"pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        KEY => "Wert",
        _ => return None,
    })
}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("neither a string"), "{error}");
    }

    #[test]
    fn a_literal_call_key_is_found_and_a_variable_is_not() {
        let text = "fn draw(name: &str) {\n    let a = t(\"Queue\");\n    let b = tf(\"Held {id}\", &[\"id\"]);\n    let c = t(name);\n    let d = i18n::t(\"Nested\");\n    let e = other.t(\"Method\");\n    let f = t_in(locale, \"Late\");\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Queue".to_owned(), "src.rs:2".to_owned()),
                ("Held {id}".to_owned(), "src.rs:3".to_owned()),
                ("Nested".to_owned(), "src.rs:5".to_owned()),
                ("Late".to_owned(), "src.rs:7".to_owned()),
            ]
        );
    }

    /// A key argument collects the literal of every branch it can take, and a
    /// branch that is not a literal is reported.
    #[test]
    fn the_literals_of_every_branch_are_keys() {
        let text = "fn draw(preset: Preset, confirm: bool, name: &str) {\n    let a = t(match preset {\n        Preset::ReadOnly => \"Read only\",\n        Preset::FullAccess => {\n            \"Full access\"\n        }\n    });\n    let b = tf(if confirm { \"Held {id}\" } else { \"Released {id}\" }, &[\"id\"]);\n    let c = t((\"Parenthesized\"));\n    let d = t(name);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Read only".to_owned(), "src.rs:3".to_owned()),
                ("Full access".to_owned(), "src.rs:5".to_owned()),
                ("Held {id}".to_owned(), "src.rs:8".to_owned()),
                ("Released {id}".to_owned(), "src.rs:8".to_owned()),
                ("Parenthesized".to_owned(), "src.rs:9".to_owned()),
            ]
        );
        assert_eq!(keys.unreadable, vec!["src.rs:10".to_owned()]);
    }

    /// The title, loading, empty and no-match text a `ListPanel` holds until it
    /// draws are keys; its hint and its other constructor arguments are not.
    #[test]
    fn the_texts_a_list_panel_translates_when_it_draws_are_keys() {
        let text = "fn panel(hint: &str) {\n    let mut list = ListPanel::new(\"Title\", hint, \"Loading...\", \"Empty.\", \"No match.\", false);\n    list.set_empty_text(\"Nothing here.\");\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Title".to_owned(), "src.rs:2".to_owned()),
                ("Loading...".to_owned(), "src.rs:2".to_owned()),
                ("Empty.".to_owned(), "src.rs:2".to_owned()),
                ("No match.".to_owned(), "src.rs:2".to_owned()),
                ("Nothing here.".to_owned(), "src.rs:3".to_owned()),
            ]
        );
    }

    /// A root whose Simplified Chinese table holds `table` and whose one source
    /// file passes `source`, so `check` reads both.
    fn root(name: &str, table: &str, source: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("dim-xtask-i18n-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let areas = root.join(TABLES).join(BASE);
        std::fs::create_dir_all(&areas).unwrap();
        std::fs::write(areas.join("panel.rs"), table).unwrap();
        let sources = root.join(SOURCE).join("ui");
        std::fs::create_dir_all(&sources).unwrap();
        std::fs::write(sources.join("panel.rs"), source).unwrap();
        root
    }

    fn refusal(root: &Path) -> String {
        let error = check(root)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        std::fs::remove_dir_all(root).unwrap();
        error
    }

    const TABLE: &str = r#"pub fn lookup(english: &str) -> Option<&'static str> {
    Some(match english {
        "Kept" => "保留",
        _ => return None,
    })
}"#;

    #[test]
    fn a_key_missing_from_a_match_branch_is_refused_by_name() {
        let root = root(
            "match-branch",
            TABLE,
            "fn draw(preset: Preset) {\n    let label = t(match preset {\n        Preset::A => \"Kept\",\n        Preset::B => \"Missing from a match branch\",\n    });\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:4: \"Missing from a match branch\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_key_missing_from_an_if_branch_is_refused_by_name() {
        let root = root(
            "if-branch",
            TABLE,
            "fn draw(confirm: bool) {\n    let label = t(if confirm {\n        \"Kept\"\n    } else {\n        \"Missing from an if branch\"\n    });\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:5: \"Missing from an if branch\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    /// A `const` of the same file is read as its literal; a path to another
    /// file and a parameter are not.
    #[test]
    fn a_constant_of_the_same_file_is_read_and_a_parameter_is_not() {
        let text = "const HINT: &str = \"Kept\";\n\nfn footer(hint: &str) {\n    let a = t(HINT);\n    let b = t(crate::report::HINT);\n    let c = t(hint);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, vec![("Kept".to_owned(), "src.rs:4".to_owned())]);
        assert_eq!(
            keys.unreadable,
            vec!["src.rs:5".to_owned(), "src.rs:6".to_owned()]
        );
    }

    #[test]
    fn a_key_missing_from_a_constant_is_refused_by_name() {
        let root = root(
            "constant-key",
            TABLE,
            "const HINT: &str = \"Missing from a constant\";\n\nfn footer() {\n    let hint = t(HINT);\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:4: \"Missing from a constant\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    #[test]
    fn a_key_missing_from_a_macro_is_refused_by_name() {
        let root = root(
            "macro-key",
            TABLE,
            "fn draw() {\n    let rows = vec![t(\"Kept\"), t(\"Missing from a macro\")];\n    drop(rows);\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:2: \"Missing from a macro\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    /// A macro that is a statement holds keys the same way an expression does.
    #[test]
    fn a_key_missing_from_a_statement_macro_is_refused_by_name() {
        let root = root(
            "statement-macro",
            TABLE,
            "fn draw() {\n    println!(\"{} {}\", t(\"Kept\"), t(\"Missing from a statement macro\"));\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:2: \"Missing from a statement macro\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    /// An item macro holds keys like any other macro call.
    #[test]
    fn a_key_missing_from_an_item_macro_is_refused_by_name() {
        let root = root(
            "item-macro",
            TABLE,
            "rows!(t(\"Missing from an item macro\"));\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:1: \"Missing from an item macro\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    /// A macro whose tokens are not expressions is one key argument the scan
    /// cannot read when they hold a translation call, and no argument when they
    /// do not.
    #[test]
    fn a_macro_the_scan_cannot_parse_is_counted_when_it_holds_a_translation_call() {
        let counted = call_keys(
            "fn draw() {\n    table! {\n        header: t(\"Unparsed key\"),\n    }\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(counted.keys, Vec::new(), "no key is read from the tokens");
        assert_eq!(counted.unreadable, vec!["src.rs:3".to_owned()]);

        let quiet = call_keys(
            "fn draw(x: Option<u8>) {\n    let a = matches!(x, Some(_));\n}\n",
            "src.rs",
        )
        .unwrap();
        assert!(quiet.unreadable.is_empty(), "{:?}", quiet.unreadable);
    }

    /// A parameter that takes a constant's name hides it, so the scan reads no
    /// key rather than the constant's text.
    #[test]
    fn a_parameter_that_hides_a_constant_is_not_read_as_its_key() {
        let text =
            "const HINT: &str = \"Kept\";\n\nfn footer(HINT: &str) {\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a parameter is not the constant");
        assert_eq!(keys.unreadable, vec!["src.rs:4".to_owned()]);
    }

    /// Two modules may hold a constant of the same name: a call site reads the
    /// one its own module declares.
    #[test]
    fn a_constant_is_read_from_the_module_of_the_call_site() {
        let text = "mod a {\n    const HINT: &str = \"A key\";\n\n    fn draw() {\n        let a = t(HINT);\n    }\n}\n\nmod b {\n    const HINT: &str = \"B key\";\n\n    fn draw() {\n        let b = t(HINT);\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("A key".to_owned(), "src.rs:5".to_owned()),
                ("B key".to_owned(), "src.rs:13".to_owned()),
            ]
        );
    }

    /// A `#[cfg(test)]` item holds a fixture the interface never draws, so the
    /// check reads no key from it.
    #[test]
    fn a_text_under_cfg_test_is_not_a_key() {
        let root = root(
            "cfg-test",
            TABLE,
            "fn panel() {\n    let list = ListPanel::new(\"Kept\", \"h\", \"\", \"\", \"\", false);\n    drop(list);\n}\n\n#[cfg(test)]\nfn fixture() {\n    let list = ListPanel::new(\"T\", \"h\", \"loading\", \"empty\", \"none\", false);\n    drop(list);\n}\n\n#[cfg(test)]\nmod tests {\n    fn draws() {\n        let list = ListPanel::new(\"U\", \"h\", \"waiting\", \"blank\", \"other\", false);\n        drop(list);\n    }\n}\n",
        );
        assert_eq!(refusal(&root), "", "a fixture text is not a key");
    }

    #[test]
    fn a_panel_text_missing_from_the_base_table_is_refused_by_name() {
        let root = root(
            "panel-text",
            TABLE,
            "fn panel(hint: &str) {\n    let list = ListPanel::new(\"Missing panel title\", hint, \"\", \"\", \"\", false);\n    drop(list);\n}\n",
        );
        let error = refusal(&root);
        assert!(
            error.contains(
                "crates/dim-cli/src/ui/panel.rs:2: \"Missing panel title\" is not in zh_cn"
            ),
            "{error}"
        );
    }

    /// An inline module does not see a constant outside it, so a name it does
    /// not declare is not read as one.
    #[test]
    fn a_module_does_not_read_a_constant_outside_it() {
        let text = "const HINT: &str = \"Outer key\";\n\nmod inner {\n    fn draw() {\n        let a = t(HINT);\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            Vec::new(),
            "a module does not see an outer constant"
        );
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);

        let imported = call_keys(
            "const HINT: &str = \"Outer key\";\n\nmod inner {\n    use crate::other::HINT;\n\n    fn draw() {\n        let a = t(HINT);\n    }\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(
            imported.keys,
            Vec::new(),
            "the import is not the outer constant"
        );
        assert_eq!(imported.unreadable, vec!["src.rs:7".to_owned()]);
    }

    /// A `use` that imports a name hides a constant of that name in the scope
    /// it is written in.
    #[test]
    fn a_use_import_hides_a_constant_of_the_same_name() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    use crate::other::HINT;\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "an imported name is not a constant");
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// `use ... as` binds the new name, and `use ...::{self}` binds the parent
    /// segment.
    #[test]
    fn a_use_rename_and_a_self_import_hide_a_constant() {
        let renamed = call_keys(
            "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    use other::B as HINT;\n    let a = t(HINT);\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(
            renamed.keys,
            Vec::new(),
            "a renamed import is not a constant"
        );
        assert_eq!(renamed.unreadable, vec!["src.rs:5".to_owned()]);

        let parent = call_keys(
            "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    use other::HINT::{self};\n    let a = t(HINT);\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(parent.keys, Vec::new(), "`self` binds the parent segment");
        assert_eq!(parent.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// A glob import may bring a constant into the scope, so an outer name is
    /// not read there; a constant the same scope declares still is.
    #[test]
    fn a_glob_import_hides_an_outer_constant_but_not_a_local_one() {
        let text = "const OUTER: &str = \"Outer key\";\n\nfn draw() {\n    use other::*;\n    let a = t(OUTER);\n}\n\nmod kept {\n    const LOCAL: &str = \"Local key\";\n    use other::*;\n\n    fn draw() {\n        let b = t(LOCAL);\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![("Local key".to_owned(), "src.rs:13".to_owned())]
        );
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// A static declared outside the function hides a constant of the same
    /// name in the module.
    #[test]
    fn a_foreign_static_hides_a_constant_of_the_same_name() {
        let text = "const HINT: &str = \"Outer key\";\n\nextern \"C\" {\n    static HINT: i32;\n}\n\nfn draw() {\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a foreign static is not a constant");
        assert_eq!(keys.unreadable, vec!["src.rs:8".to_owned()]);
    }

    /// A static in a block hides a constant of the same name there.
    #[test]
    fn a_local_static_hides_a_constant_of_the_same_name() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    static HINT: &str = \"A static\";\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a static is not a constant");
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// An `if let` binding hides a constant in the `then` branch; the `else`
    /// branch is outside the binding and still reads the constant.
    #[test]
    fn an_if_let_binding_hides_a_constant_and_the_else_branch_reads_it() {
        let text = "const HINT: &str = \"Kept\";\n\nfn draw(x: Option<&str>) {\n    let a = if let Some(HINT) = x {\n        t(HINT)\n    } else {\n        t(HINT)\n    };\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, vec![("Kept".to_owned(), "src.rs:7".to_owned())]);
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// A `while let` binding hides a constant in the body only.
    #[test]
    fn a_while_let_binding_hides_a_constant() {
        let text = "const HINT: &str = \"Kept\";\n\nfn draw(mut it: It) {\n    while let Some(HINT) = next(t(HINT)) {\n        t(HINT);\n    }\n    t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Kept".to_owned(), "src.rs:4".to_owned()),
                ("Kept".to_owned(), "src.rs:7".to_owned()),
            ]
        );
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// A `for` binding hides a constant in the body, not in the expression its
    /// loop iterates.
    #[test]
    fn a_for_binding_does_not_hide_a_constant_from_its_iterable() {
        let text = "const HINT: &str = \"Kept\";\n\nfn draw() {\n    for HINT in [t(HINT)] {\n        t(HINT);\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, vec![("Kept".to_owned(), "src.rs:4".to_owned())]);
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);
    }

    /// A `match` arm binds its pattern for the arm body; a binding an `if let`
    /// guard introduces stays inside the guard.
    #[test]
    fn a_match_arm_binding_hides_a_constant_and_a_guard_binding_stays_inside() {
        let text = "const HINT: &str = \"Kept\";\n\nfn draw(x: Option<&str>) {\n    if let Some(HINT) = x {\n        t(HINT);\n    }\n    match x {\n        Some(v) if let Some(HINT) = v => t(HINT),\n        Some(HINT) => t(HINT),\n        _ => t(HINT),\n    };\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Kept".to_owned(), "src.rs:8".to_owned()),
                ("Kept".to_owned(), "src.rs:10".to_owned()),
            ]
        );
        assert_eq!(
            keys.unreadable,
            vec!["src.rs:5".to_owned(), "src.rs:9".to_owned()]
        );
    }

    /// A binding on both sides of a subpattern hides a constant, including the
    /// names the subpattern binds.
    #[test]
    fn a_binding_at_a_subpattern_hides_a_constant() {
        let text = "const INNER: &str = \"Kept\";\n\nfn draw(x: Option<Option<&str>>) {\n    if let HINT @ Some(INNER) = x {\n        t(HINT);\n        t(INNER);\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "both bindings hide the constant");
        assert_eq!(
            keys.unreadable,
            vec!["src.rs:5".to_owned(), "src.rs:6".to_owned()]
        );
    }

    /// A pattern macro may bind a name the scan cannot see, so no outer name is
    /// read after it.
    #[test]
    fn a_pattern_macro_hides_a_constant() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw(value: Value) {\n    let bound!(HINT) = value;\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a pattern macro may bind the name");
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);

        let tokens = call_keys(
            "fn draw(value: Value) {\n    let bound!(t(\"Pattern key\")) = value;\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(
            tokens.keys,
            vec![("Pattern key".to_owned(), "src.rs:2".to_owned())]
        );
    }

    /// A macro in statement position may declare a name the scan cannot see,
    /// so no outer name is read after it.
    #[test]
    fn a_statement_macro_hides_a_constant() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    bind_hint!(HINT);\n    let a = t(HINT);\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a statement macro may bind the name");
        assert_eq!(keys.unreadable, vec!["src.rs:5".to_owned()]);

        let local = call_keys(
            "fn draw() {\n    const HINT: &str = \"Block key\";\n    bind_hint!();\n    let a = t(HINT);\n}\n",
            "src.rs",
        )
        .unwrap();
        assert_eq!(
            local.keys,
            Vec::new(),
            "the macro may bind the block's name"
        );
        assert_eq!(local.unreadable, vec!["src.rs:4".to_owned()]);
    }

    /// A translation call inside a `macro_rules!` definition is one argument
    /// the scan cannot read, not a silent skip.
    #[test]
    fn a_translation_call_inside_a_macro_rules_definition_is_counted() {
        let text = "fn draw() {\n    macro_rules! warn_row {\n        () => {\n            t(\"Macro rules key\")\n        };\n    }\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "no key is read from a definition");
        assert_eq!(keys.unreadable, vec!["src.rs:3".to_owned()]);
    }

    /// A block a key argument evaluates to reads the constant it declares, not
    /// the constant of the same name outside it.
    #[test]
    fn a_block_key_argument_reads_the_constant_it_declares() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw() {\n    let a = t({\n        const HINT: &str = \"Inner key\";\n        HINT\n    });\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![("Inner key".to_owned(), "src.rs:6".to_owned())]
        );
        assert_eq!(
            keys.unreadable,
            Vec::<String>::new(),
            "the inner constant is read"
        );
    }

    /// A block a key argument evaluates to reads the binding it declares, which
    /// is not a constant, so the argument is counted instead.
    #[test]
    fn a_block_key_argument_reads_the_binding_it_declares() {
        let text = "const HINT: &str = \"Kept\";\n\nfn draw(q: &str) {\n    let a = t({\n        let HINT = q;\n        HINT\n    });\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "a binding is not the constant");
        assert_eq!(keys.unreadable, vec!["src.rs:6".to_owned()]);
    }

    /// A block in a `match` arm of a key argument reads its own constant, while
    /// an arm that declares none still reads the outer one.
    #[test]
    fn a_match_arm_block_key_argument_reads_its_own_constant() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw(k: K) {\n    let a = t(match k {\n        K::A => {\n            const HINT: &str = \"Inner key\";\n            HINT\n        }\n        K::B => HINT,\n    });\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Inner key".to_owned(), "src.rs:7".to_owned()),
                ("Outer key".to_owned(), "src.rs:9".to_owned()),
            ]
        );
        assert_eq!(keys.unreadable, Vec::<String>::new());
    }

    /// A branch of an `if` key argument opens the scope of a block it holds, and
    /// a block inside that block opens its own.
    #[test]
    fn an_if_branch_of_a_key_argument_reads_a_nested_block_scope() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw(c: bool) {\n    let a = t(if c {\n        const HINT: &str = \"Inner key\";\n        { HINT }\n    } else {\n        HINT\n    });\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(
            keys.keys,
            vec![
                ("Inner key".to_owned(), "src.rs:6".to_owned()),
                ("Outer key".to_owned(), "src.rs:8".to_owned()),
            ]
        );
        assert_eq!(keys.unreadable, Vec::<String>::new());
    }

    /// A `use`, a `static` and a macro in a block a key argument evaluates to
    /// each hide the constant of the same name, so the argument is counted.
    #[test]
    fn a_use_static_or_macro_in_a_key_argument_block_hides_the_constant() {
        let text = "const HINT: &str = \"Outer key\";\n\nfn draw(q: Q) {\n    let a = t({ use other::HINT; HINT });\n    let b = t({ static HINT: &str = \"A static\"; HINT });\n    let c = t({ bind_hint!(); HINT });\n}\n";
        let keys = call_keys(text, "src.rs").unwrap();
        assert_eq!(keys.keys, Vec::new(), "no outer constant is read");
        assert_eq!(
            keys.unreadable,
            vec![
                "src.rs:4".to_owned(),
                "src.rs:5".to_owned(),
                "src.rs:6".to_owned(),
            ]
        );
    }
}
