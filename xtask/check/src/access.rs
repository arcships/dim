//! The access-layer rules of [#2015 §5](https://github.com/arcships/dim/issues/2015)
//! and the list that admits the violations still in the tree.
//!
//! A violation is one of:
//!
//! - `#[service(skip)]` on a method of a `#[rutis::service]` block;
//! - a method of a service type that takes `self` and is `pub` or
//!   `pub(crate)`, written outside its `#[rutis::service]` blocks, or such a
//!   field of a service type: Rust callers can use what other languages cannot,
//!   which is what `#[service(skip)]` does. A constructor takes no `self` and
//!   is not one of these, and a type that serializes has fields of data, the
//!   payload a face carries, so its fields are not either;
//! - a hand-written `HostDispatch` implementation;
//! - a `Session` method that takes a line's name, the forwarding shape the
//!   refactor removes. It reads the parameter's name (heuristic: the tree's
//!   forwarding methods name it `branch`), so a method that takes a line under
//!   another name is not found;
//! - the outward `Branch` type.
//!
//! The scan reads the product sources (`crates/*/src`) only: a test fixture is
//! not a public face.
//!
//! The allowlist (`xtask/access-layer.json`) names every violation the tree
//! still has. `check` fails when a violation is not listed, and when a listed
//! entry matches fewer occurrences than it is listed for; it cannot see a
//! violation that arrives together with its own entry. Keeping the list from
//! growing is a review rule: a change deletes the entries for what it removes
//! and adds none, so the list empties when the refactor ends. The same file's
//! `removed` list names catalog services a change deleted; the runtime catalog
//! check in `xtask` fails when one comes back.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The file holding the allowlist and the removed names, from the workspace
/// root.
pub const FILE: &str = "xtask/access-layer.json";

/// What kind of access-layer violation a rule names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// A service method kept out of the face with `#[service(skip)]`.
    ServiceSkip,
    /// A `pub`/`pub(crate)` method that takes `self` on a service type,
    /// outside its `#[rutis::service]` blocks.
    RustOnlyMethod,
    /// A `pub`/`pub(crate)` field of a service type.
    RustOnlyField,
    /// A `HostDispatch` implementation written by hand instead of a
    /// `#[rutis::service]` block.
    HostDispatch,
    /// A `Session` method that forwards to a line named by an argument.
    SessionForwarding,
    /// The outward `Branch` type.
    BranchType,
}

/// One violation, identified by what it is and where it lives, not by line
/// numbers.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub kind: Kind,
    /// The source file, from the workspace root.
    pub file: String,
    /// The item: `Type::method` for a method or field, the type for a type.
    pub item: String,
}

impl std::fmt::Display for Rule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {} is {}",
            self.file,
            self.item,
            kind_name(self.kind)
        )
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::ServiceSkip => "skipped with #[service(skip)]",
        Kind::RustOnlyMethod => "a Rust-only method",
        Kind::RustOnlyField => "a Rust-only field",
        Kind::HostDispatch => "a hand-written HostDispatch",
        Kind::SessionForwarding => "a Session method taking a line name",
        Kind::BranchType => "the outward Branch type",
    }
}

/// The allowlist and the removed names.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccessLayer {
    /// Violations the tree still has, one entry per occurrence.
    #[serde(default)]
    pub allow: Vec<Rule>,
    /// Catalog services a change deleted. Empty until the first face goes.
    #[serde(default)]
    pub removed: Vec<String>,
}

/// Reads the file at `root`; a missing file admits nothing.
pub fn load(root: &Path) -> Result<AccessLayer> {
    let path = root.join(FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            Ok(serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", path.display()))?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AccessLayer::default()),
        Err(error) => Err(format!("{}: {error}", path.display()).into()),
    }
}

/// Fails when a violation is not allowed, or an entry matches fewer
/// occurrences than it is listed for.
pub fn check(root: &Path) -> Result<()> {
    let mut found: BTreeMap<Rule, usize> = BTreeMap::new();
    for rule in violations(root)? {
        *found.entry(rule).or_default() += 1;
    }
    let mut allowed: BTreeMap<Rule, usize> = BTreeMap::new();
    for rule in load(root)?.allow {
        *allowed.entry(rule).or_default() += 1;
    }
    let mut problems = String::new();
    for (rule, count) in &found {
        let listed = allowed.get(rule).copied().unwrap_or(0);
        if listed < *count {
            problems += &format!(
                "{} ({count} found, {listed} listed)\n",
                serde_json::to_string(rule).expect("a rule is JSON")
            );
        }
    }
    if !problems.is_empty() {
        return Err(format!(
            "access-layer violations are not in {FILE}; the list may only shrink:\n{problems}"
        )
        .into());
    }
    let mut stale = String::new();
    for (rule, listed) in &allowed {
        let count = found.get(rule).copied().unwrap_or(0);
        if count < *listed {
            stale += &format!("{rule} (listed {listed}, {count} found)\n");
        }
    }
    if !stale.is_empty() {
        return Err(format!(
            "entries of {FILE} match fewer occurrences and must be deleted:\n{stale}"
        )
        .into());
    }
    println!(
        "access layer passed ({} allowed)",
        allowed.values().sum::<usize>()
    );
    Ok(())
}

/// Every violation in the product sources under `root`.
pub fn violations(root: &Path) -> Result<Vec<Rule>> {
    let mut parsed = Vec::new();
    for file in sources(root)? {
        let text = std::fs::read_to_string(&file)?;
        let ast = syn::parse_file(&text).map_err(|error| format!("{}: {error}", file.display()))?;
        let relative = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        parsed.push((relative, ast));
    }
    let mut services = BTreeSet::new();
    for (file, ast) in &parsed {
        service_types(crate_name(file), &ast.items, &mut services);
    }
    let mut found = Vec::new();
    for (file, ast) in &parsed {
        scan(&ast.items, file, crate_name(file), &services, &mut found);
    }
    found.sort();
    Ok(found)
}

/// The types that have a `#[rutis::service]` block, by crate: what they offer
/// other languages is that block, and nothing else.
fn service_types(crate_name: &str, items: &[syn::Item], services: &mut BTreeSet<(String, String)>) {
    for item in items {
        match item {
            syn::Item::Impl(block) if block.attrs.iter().any(is_service_attribute) => {
                services.insert((crate_name.to_owned(), type_name(&block.self_ty)));
            }
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    service_types(crate_name, items, services);
                }
            }
            _ => {}
        }
    }
}

/// The product source files: every `.rs` under `crates/*/src`.
fn sources(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let crates = root.join("crates");
    for entry in std::fs::read_dir(&crates)? {
        let directory = entry?.path().join("src");
        if directory.is_dir() {
            collect(&directory, &mut files)?;
        }
    }
    files.sort();
    Ok(files)
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn scan(
    items: &[syn::Item],
    file: &str,
    crate_name: &str,
    services: &BTreeSet<(String, String)>,
    found: &mut Vec<Rule>,
) {
    let declared = |ty: &str| services.contains(&(crate_name.to_owned(), ty.to_owned()));
    let rule = |kind, item: String| Rule {
        kind,
        file: file.to_owned(),
        item,
    };
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    scan(items, file, crate_name, services, found);
                }
            }
            syn::Item::Struct(structure)
                if declared(&structure.ident.to_string()) && !serializes(&structure.attrs) =>
            {
                for (index, field) in structure.fields.iter().enumerate() {
                    if !outward(&field.vis) {
                        continue;
                    }
                    let name = match &field.ident {
                        Some(ident) => ident.to_string(),
                        None => index.to_string(),
                    };
                    found.push(rule(
                        Kind::RustOnlyField,
                        format!("{}::{name}", structure.ident),
                    ));
                }
            }
            syn::Item::Impl(block) => {
                let ty = type_name(&block.self_ty);
                let service = block.attrs.iter().any(is_service_attribute);
                for method in &block.items {
                    let syn::ImplItem::Fn(method) = method else {
                        continue;
                    };
                    if service {
                        if method
                            .attrs
                            .iter()
                            .any(|attr| is_service_attribute(attr) && skips(attr))
                        {
                            found.push(rule(
                                Kind::ServiceSkip,
                                format!("{ty}::{}", method.sig.ident),
                            ));
                        }
                    } else if declared(&ty)
                        && outward(&method.vis)
                        && method.sig.receiver().is_some()
                    {
                        found.push(rule(
                            Kind::RustOnlyMethod,
                            format!("{ty}::{}", method.sig.ident),
                        ));
                    }
                }
                if block
                    .trait_
                    .as_ref()
                    .and_then(|(_, path, _)| path.segments.last())
                    .is_some_and(|segment| segment.ident == "HostDispatch")
                {
                    found.push(rule(Kind::HostDispatch, ty.clone()));
                }
                if block.trait_.is_none() && ty == "Session" {
                    for method in &block.items {
                        let syn::ImplItem::Fn(method) = method else {
                            continue;
                        };
                        let named_line = method.sig.inputs.iter().any(|input| {
                            matches!(input, syn::FnArg::Typed(typed)
                                if matches!(&*typed.pat, syn::Pat::Ident(ident) if ident.ident == "branch"))
                        });
                        if named_line {
                            found.push(rule(
                                Kind::SessionForwarding,
                                format!("Session::{}", method.sig.ident),
                            ));
                        }
                    }
                }
            }
            syn::Item::Struct(item)
                if matches!(item.vis, syn::Visibility::Public(_)) && item.ident == "Branch" =>
            {
                found.push(rule(Kind::BranchType, "Branch".into()));
            }
            syn::Item::Enum(item)
                if matches!(item.vis, syn::Visibility::Public(_)) && item.ident == "Branch" =>
            {
                found.push(rule(Kind::BranchType, "Branch".into()));
            }
            _ => {}
        }
    }
}

/// The crate a product source file belongs to, as its directory name.
fn crate_name(file: &str) -> &str {
    file.strip_prefix("crates/")
        .and_then(|rest| rest.split_once('/'))
        .map(|(name, _)| name)
        .unwrap_or(file)
}

/// Whether a type serializes: its fields are the data a face carries, which
/// other languages read and write, not state a caller reaches around the face.
fn serializes(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("derive")
            && attr
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                )
                .is_ok_and(|derives| {
                    derives.iter().any(|derive| {
                        derive
                            .segments
                            .last()
                            .is_some_and(|segment| segment.ident == "Serialize")
                    })
                })
    })
}

/// Whether a visibility lets a caller in another module of this crate use the
/// item: `pub` or `pub(crate)`.
fn outward(visibility: &syn::Visibility) -> bool {
    match visibility {
        syn::Visibility::Public(_) => true,
        syn::Visibility::Restricted(restricted) => restricted.path.is_ident("crate"),
        syn::Visibility::Inherited => false,
    }
}

/// `#[service]`, `#[service(skip)]` or their `rutis::` spellings.
fn is_service_attribute(attr: &syn::Attribute) -> bool {
    path_is_service(attr.path())
}

fn path_is_service(path: &syn::Path) -> bool {
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    matches!(segments.as_slice(), [service] if service == "service")
        || matches!(segments.as_slice(), [root, service] if root == "rutis" && service == "service")
}

fn skips(attr: &syn::Attribute) -> bool {
    attr.parse_args::<syn::Ident>()
        .is_ok_and(|argument| argument == "skip")
}

/// The last path segment of a type, `Branch` for `crate::Branch`.
fn type_name(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}
