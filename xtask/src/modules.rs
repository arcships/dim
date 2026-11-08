//! Source-derived module graph. This is a conservative syntax analysis, not a
//! replacement for rustc name resolution or macro expansion.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use syn::{Item, UseTree, visit::Visit};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
pub(crate) const TYPES_VALUE_DEPENDENCIES: &[&str] = &["serde", "serde_json"];
/// Crates below the app-server composition layer. Cargo itself rejects normal
/// cyclic package dependencies; this check mainly guards dev-dependencies,
/// which cargo allows in cycles.
pub(crate) const APP_SERVER_COMPOSITION_BELOW: &[&str] = &[
    "dim-agent-runtime",
    "dim-agent-capabilities",
    "dim-agent-aimux",
    "dim-agent-attachments",
    "dim-agent-questions",
];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Symbol {
    Module(String),
    Item(String),
    External(String),
}
#[derive(Clone)]
struct Import {
    path: Vec<String>,
    alias: Option<String>,
    glob: bool,
}
struct Module {
    root: String,
    parent: Option<String>,
    file: String,
    definitions: BTreeMap<String, Symbol>,
    imports: Vec<Import>,
    paths: BTreeSet<Vec<String>>,
    macros: BTreeSet<String>,
    /// Implements BusinessDocument: the module owns a business source.
    business_document: bool,
}
#[derive(Default)]
struct SourceGraph {
    modules: BTreeMap<String, Module>,
    dependencies: BTreeMap<String, BTreeMap<String, String>>,
    edges: BTreeMap<(String, String), BTreeSet<String>>,
    unresolved: BTreeSet<(String, String)>,
    symbols: BTreeMap<String, BTreeMap<String, BTreeSet<Symbol>>>,
}

/// Analyze production crate source, emit deterministic JSON/Mermaid and reject
/// architectural violations. All edges come from source declarations/references.
/// Returns the rendered `modules.json` and `modules.mmd`; the caller writes or verifies them.
pub fn generate(workspace: &Path) -> Result<Vec<(&'static str, String)>> {
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(workspace)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata packages missing")?;
    let products: Vec<_> = packages
        .iter()
        .filter(|p| {
            p["name"].as_str().is_some_and(|name| {
                name.starts_with("dim-agent-")
                    || matches!(
                        name,
                        "dim-app-server"
                            | "dim-client"
                            | "dim-acp"
                            | "dim-cli"
                            | "dim-migration"
                            | "dim-session-title"
                            | "dim-goal"
                            | "dim-modality"
                            | "dim-ocr"
                    )
            })
        })
        .collect();
    let mut graph = SourceGraph::default();
    for package in &products {
        let name = package["name"].as_str().ok_or("package name missing")?;
        if name == "dim-agent-types" {
            for dependency in package["dependencies"]
                .as_array()
                .ok_or("dependencies missing")?
            {
                let dependency_name = dependency["name"]
                    .as_str()
                    .ok_or("dependency name missing")?;
                if !TYPES_VALUE_DEPENDENCIES.contains(&dependency_name) {
                    return Err(format!("dim-agent-types dependency {dependency_name} is outside the reviewed value-library allowlist").into());
                }
            }
        }
        if APP_SERVER_COMPOSITION_BELOW.contains(&name) {
            for dependency in package["dependencies"]
                .as_array()
                .ok_or("dependencies missing")?
            {
                if dependency["name"].as_str() == Some("dim-app-server") {
                    return Err(format!(
                        "{name} dependency dim-app-server crosses the app-server composition layer"
                    )
                    .into());
                }
            }
        }
        let library = name.replace('-', "_");
        for target in package["targets"].as_array().ok_or("targets missing")? {
            let kinds = target["kind"].as_array().ok_or("target kinds missing")?;
            let is_lib = kinds
                .iter()
                .any(|kind| matches!(kind.as_str(), Some("lib" | "rlib")));
            let is_bin = kinds.iter().any(|kind| kind == "bin");
            if !is_lib && !is_bin {
                continue;
            }
            let root = if is_lib {
                library.clone()
            } else {
                format!(
                    "{library}::@bin:{}",
                    target["name"].as_str().ok_or("target name missing")?
                )
            };
            let mut dependencies = BTreeMap::new();
            for dependency in package["dependencies"]
                .as_array()
                .ok_or("dependencies missing")?
            {
                let dep_name = dependency["name"]
                    .as_str()
                    .ok_or("dependency name missing")?;
                let alias = dependency["rename"]
                    .as_str()
                    .unwrap_or(dep_name)
                    .replace('-', "_");
                dependencies.insert(alias, dep_name.replace('-', "_"));
            }
            dependencies.insert(library.clone(), library.clone());
            graph.dependencies.insert(root.clone(), dependencies);
            let file = PathBuf::from(target["src_path"].as_str().ok_or("target source missing")?);
            let ast = syn::parse_file(&fs::read_to_string(&file)?)?;
            let directory = file
                .parent()
                .ok_or("source directory missing")?
                .to_path_buf();
            graph.read_module(&root, &root, None, &file, &directory, ast.items, workspace)?;
        }
    }
    graph.resolve_symbols()?;
    graph.resolve_edges();
    graph.check_boundaries()?;
    let nodes: Vec<_> = graph
        .modules
        .iter()
        .map(|(id, module)| {
            json!({
                "id":id,"crate":module.root,"parent":module.parent,"file":module.file,
                "unexpandedMacros":module.macros,
            })
        })
        .collect();
    let edges: Vec<_> = graph
        .edges
        .iter()
        .map(|((from, to), reasons)| json!({"from":from,"to":to,"references":reasons}))
        .collect();
    let report = json!({
        "analysis": {
            "source":"syn AST: mod declarations, use trees, item definitions and symbol paths",
            "scope":"dim-agent-*, dim-app-server, dim-client, dim-acp, dim-cli, dim-migration, dim-session-title and dim-goal library/binary targets; cfg branches are conservatively included",
            "glob":"glob/re-export chains are resolved for referenced names; an unused glob does not create every possible dependency",
            "limitations":["Macros are not expanded; unexpandedMacros names are reported per module", "Method receiver types, local-variable shadowing and trait-associated item inference are not resolved", "Qualified unresolved paths are reported; this graph supplements cargo check and is not complete rustc name resolution"],
            "typesAllowedDependencies":TYPES_VALUE_DEPENDENCIES,
        },
        "modules":nodes,"edges":edges,
        "unresolved":graph.unresolved.iter().map(|(module,path)|json!({"module":module,"path":path})).collect::<Vec<_>>(),
    });
    let json = serde_json::to_string_pretty(&report)? + "\n";
    let ids: BTreeMap<_, _> = graph
        .modules
        .keys()
        .enumerate()
        .map(|(index, id)| (id, format!("m{index}")))
        .collect();
    let mut mermaid = String::from(
        "%% Generated from Rust source AST; do not edit.\n%% Conservative cfg union; macros and inferred method receivers are not expanded.\nflowchart LR\n",
    );
    for (id, node) in &ids {
        mermaid.push_str(&format!("  {node}[\"{}\"]\n", escape(id)));
    }
    for ((from, to), reasons) in &graph.edges {
        let declaration = reasons.iter().all(|reason| reason.starts_with("mod "));
        mermaid.push_str(&format!(
            "  {} {} {}\n",
            ids[from],
            if declaration { "-.->" } else { "-->" },
            ids[to]
        ));
    }
    println!(
        "module source boundaries passed ({} modules, {} edges)",
        graph.modules.len(),
        graph.edges.len()
    );
    Ok(vec![("modules.json", json), ("modules.mmd", mermaid)])
}

impl SourceGraph {
    #[allow(clippy::too_many_arguments)]
    fn read_module(
        &mut self,
        id: &str,
        root: &str,
        parent: Option<String>,
        file: &Path,
        directory: &Path,
        items: Vec<Item>,
        workspace: &Path,
    ) -> Result<()> {
        if self.modules.contains_key(id) {
            return Err(format!("duplicate module identity {id}").into());
        }
        let mut module = Module {
            root: root.to_owned(),
            parent,
            file: file
                .strip_prefix(workspace)
                .unwrap_or(file)
                .to_string_lossy()
                .into_owned(),
            definitions: BTreeMap::new(),
            imports: vec![],
            paths: BTreeSet::new(),
            macros: BTreeSet::new(),
            business_document: false,
        };
        let mut children = vec![];
        for item in items {
            if let Item::Mod(child) = item {
                let name = child.ident.to_string();
                let child_id = format!("{id}::{name}");
                module
                    .definitions
                    .insert(name.clone(), Symbol::Module(child_id.clone()));
                self.edges
                    .entry((id.to_owned(), child_id.clone()))
                    .or_default()
                    .insert(format!("mod {name}"));
                if let Some((_, items)) = child.content {
                    children.push((child_id, file.to_owned(), directory.join(&name), items));
                } else {
                    let explicit = child.attrs.iter().find_map(|attr| {
                        if attr.path().is_ident("path")
                            && let syn::Meta::NameValue(value) = &attr.meta
                            && let syn::Expr::Lit(value) = &value.value
                            && let syn::Lit::Str(path) = &value.lit
                        {
                            Some(path.value())
                        } else {
                            None
                        }
                    });
                    let source = if let Some(path) = explicit {
                        directory.join(path)
                    } else {
                        let single = directory.join(format!("{name}.rs"));
                        let nested = directory.join(&name).join("mod.rs");
                        match (single.exists(),nested.exists()) {
                            (true,false)=>single,(false,true)=>nested,
                            (true,true)=>return Err(format!("ambiguous module files for {child_id}").into()),
                            _=>return Err(format!("cannot locate source for {child_id}; generated/cfg path modules require explicit analysis support").into()),
                        }
                    };
                    let ast = syn::parse_file(&fs::read_to_string(&source)?)?;
                    let child_directory = if source.file_name().is_some_and(|name| name == "mod.rs")
                    {
                        source.parent().ok_or("module parent missing")?.to_owned()
                    } else {
                        source.with_extension("")
                    };
                    children.push((child_id, source, child_directory, ast.items));
                }
                continue;
            }
            if let Item::Use(import) = &item {
                flatten_import(&import.tree, vec![], &mut module.imports);
            } else if let Some(name) = item_name(&item) {
                module.definitions.insert(name, Symbol::Item(id.to_owned()));
            }
            let mut visitor = Paths::default();
            visitor.visit_item(&item);
            module.paths.extend(visitor.paths);
            module.macros.extend(visitor.macros);
            module.business_document |= visitor.business_document;
        }
        self.modules.insert(id.to_owned(), module);
        for (child, source, child_directory, items) in children {
            self.read_module(
                &child,
                root,
                Some(id.to_owned()),
                &source,
                &child_directory,
                items,
                workspace,
            )?;
        }
        Ok(())
    }
    fn resolve_edges(&mut self) {
        let mut references = vec![];
        for (id, module) in &self.modules {
            for import in module.imports.iter().filter(|import| !import.glob) {
                references.push((id.clone(), import.path.clone(), "use"));
            }
            for path in &module.paths {
                references.push((id.clone(), path.clone(), "path"));
            }
        }
        for (from, path, kind) in references {
            let symbols = self.path(&from, &path);
            if symbols.is_empty() && path.len() > 1 {
                self.unresolved.insert((from.clone(), path.join("::")));
            }
            for symbol in symbols {
                let to = match symbol {
                    Symbol::Module(module) | Symbol::Item(module) => module,
                    Symbol::External(_) => continue,
                };
                if from != to {
                    self.edges
                        .entry((from.clone(), to))
                        .or_default()
                        .insert(format!("{kind} {}", path.join("::")));
                }
            }
        }
    }
    fn resolve_symbols(&mut self) -> Result<()> {
        self.symbols = self
            .modules
            .iter()
            .map(|(id, module)| {
                (
                    id.clone(),
                    module
                        .definitions
                        .iter()
                        .map(|(name, symbol)| (name.clone(), BTreeSet::from([symbol.clone()])))
                        .collect(),
                )
            })
            .collect();
        for _ in 0..self.modules.len().saturating_mul(4).saturating_add(8) {
            let mut next = BTreeMap::new();
            for (id, module) in &self.modules {
                let mut names: BTreeMap<String, BTreeSet<Symbol>> = module
                    .definitions
                    .iter()
                    .map(|(name, symbol)| (name.clone(), BTreeSet::from([symbol.clone()])))
                    .collect();
                for import in module.imports.iter().filter(|import| !import.glob) {
                    if let Some(alias) = &import.alias {
                        let values = self.path(id, &import.path);
                        if !values.is_empty() {
                            names.entry(alias.clone()).or_default().extend(values);
                        }
                    }
                }
                let explicit: BTreeSet<_> = names.keys().cloned().collect();
                for import in module.imports.iter().filter(|import| import.glob) {
                    for symbol in self.path(id, &import.path) {
                        if let Symbol::Module(target) = symbol
                            && let Some(exports) = self.symbols.get(&target)
                        {
                            for (name, values) in exports {
                                if !explicit.contains(name) {
                                    names
                                        .entry(name.clone())
                                        .or_default()
                                        .extend(values.iter().cloned());
                                }
                            }
                        }
                    }
                }
                next.insert(id.clone(), names);
            }
            if next == self.symbols {
                return Ok(());
            }
            self.symbols = next;
        }
        Err("module import resolution did not converge".into())
    }
    fn path(&self, owner: &str, path: &[String]) -> BTreeSet<Symbol> {
        let Some(first) = path.first() else {
            return BTreeSet::new();
        };
        let Some(module) = self.modules.get(owner) else {
            return BTreeSet::new();
        };
        let mut symbols = match first.as_str() {
            "crate" => BTreeSet::from([Symbol::Module(module.root.clone())]),
            "self" => BTreeSet::from([Symbol::Module(owner.to_owned())]),
            "super" => module
                .parent
                .as_ref()
                .map(|parent| BTreeSet::from([Symbol::Module(parent.clone())]))
                .unwrap_or_default(),
            _ => {
                if let Some(local) = self.symbols.get(owner).and_then(|names| names.get(first)) {
                    local.clone()
                } else if let Some(dependency) = self
                    .dependencies
                    .get(&module.root)
                    .and_then(|deps| deps.get(first))
                {
                    BTreeSet::from([if self.modules.contains_key(dependency) {
                        Symbol::Module(dependency.clone())
                    } else {
                        Symbol::External(dependency.clone())
                    }])
                } else if first == "Self" {
                    BTreeSet::from([Symbol::Item(owner.to_owned())])
                } else if matches!(
                    first.as_str(),
                    "Box"
                        | "Vec"
                        | "String"
                        | "Option"
                        | "Result"
                        | "Into"
                        | "From"
                        | "Default"
                        | "Iterator"
                        | "u8"
                        | "u16"
                        | "u32"
                        | "u64"
                        | "u128"
                        | "usize"
                        | "i8"
                        | "i16"
                        | "i32"
                        | "i64"
                        | "i128"
                        | "isize"
                        | "f32"
                        | "f64"
                        | "str"
                        | "bool"
                ) {
                    BTreeSet::from([Symbol::External("std::prelude".into())])
                } else if matches!(first.as_str(), "std" | "core" | "alloc") {
                    BTreeSet::from([Symbol::External(first.clone())])
                } else {
                    BTreeSet::new()
                }
            }
        };
        for component in &path[1..] {
            let mut next = BTreeSet::new();
            for symbol in symbols {
                match symbol {
                    Symbol::Module(ref id) if component == "super" => {
                        if let Some(parent) = self.modules.get(id).and_then(|m| m.parent.clone()) {
                            next.insert(Symbol::Module(parent));
                        }
                    }
                    Symbol::Module(ref id) => {
                        if let Some(values) =
                            self.symbols.get(id).and_then(|names| names.get(component))
                        {
                            next.extend(values.iter().cloned());
                        }
                    }
                    other => {
                        next.insert(other);
                    }
                }
            }
            symbols = next;
        }
        symbols
    }
    fn check_boundaries(&self) -> Result<()> {
        let core = "dim_agent_runtime::core";
        for ((from, to), reasons) in &self.edges {
            if reasons.iter().all(|reason| reason.starts_with("mod ")) {
                continue;
            }
            let is_core = from == core || from.starts_with(&format!("{core}::"));
            let outer = [
                "dim_agent_runtime::agent",
                "dim_agent_runtime::assembly",
                "dim_agent_runtime::timeline_handle",
                "dim_agent_capabilities",
                "dim_app_server",
            ];
            if is_core
                && outer
                    .iter()
                    .any(|forbidden| to == forbidden || to.starts_with(&format!("{forbidden}::")))
            {
                return Err(format!(
                    "core reverse dependency: {from} -> {to} ({})",
                    reasons.iter().cloned().collect::<Vec<_>>().join(", ")
                )
                .into());
            }
            if from.starts_with("dim_agent_types")
                && (to.starts_with("dim_agent_runtime")
                    || to.starts_with("dim_agent_capabilities")
                    || to.starts_with("dim_app_server"))
            {
                return Err(format!("types reverse dependency: {from} -> {to}").into());
            }
            let within = |module: &str, root: &str| {
                module == root || module.starts_with(&format!("{root}::"))
            };
            if (within(from, "dim_app_server::settings")
                || within(from, "dim_agent_capabilities::settings"))
                && ((within(to, "dim_app_server")
                    && !within(to, "dim_app_server::settings")
                    && !within(to, "dim_app_server::storage"))
                    || (within(to, "dim_agent_capabilities")
                        && !within(to, "dim_agent_capabilities::file_storage")
                        && !within(to, "dim_agent_capabilities::settings")))
            {
                return Err(format!("settings business dependency: {from} -> {to}").into());
            }
            if within(from, "dim_app_server::protocol")
                && within(to, "dim_app_server")
                && (within(to, "dim_app_server::skills_management")
                    || to.split("::").any(|part| part == "management"))
            {
                return Err(format!("protocol management dependency: {from} -> {to}").into());
            }
            if let Some(rule) = cli_layer_violation(from, to) {
                return Err(format!(
                    "dim-cli layer dependency ({rule}): {from} -> {to} ({})",
                    reasons.iter().cloned().collect::<Vec<_>>().join(", ")
                )
                .into());
            }
        }
        if let Some(id) = self.modules.iter().find_map(|(id, module)| {
            (module.business_document
                && (id == "dim_app_server::settings"
                    || id.starts_with("dim_app_server::settings::")))
            .then_some(id)
        }) {
            return Err(format!("settings business document: {id}").into());
        }
        Ok(())
    }
}
/// The layers of dim-cli: shared modules sit below both clients, the
/// terminal UI does not reach into the line client, text measurement is a
/// leaf, rendering does not know the transcript, and the parts of the UI
/// do not reach up into the App that owns them. A violation names its rule.
fn cli_layer_violation(from: &str, to: &str) -> Option<&'static str> {
    let within =
        |module: &str, root: &str| module == root || module.starts_with(&format!("{root}::"));
    if !within(from, "dim_cli") || !within(to, "dim_cli") || to == "dim_cli" {
        return None;
    }
    const LINE_CLIENT: &[&str] = &[
        "dim_cli::session",
        "dim_cli::display",
        "dim_cli::input",
        "dim_cli::composer",
    ];
    const SHARED: &[&str] = &[
        "dim_cli::attachments",
        "dim_cli::clipboard",
        "dim_cli::images",
        "dim_cli::client",
        "dim_cli::wire",
        "dim_cli::mcp",
        "dim_cli::configure",
        "dim_cli::model",
    ];
    let into = |roots: &[&str]| roots.iter().any(|root| within(to, root));
    let from_in = |roots: &[&str]| roots.iter().any(|root| within(from, root));
    if from_in(SHARED) && (within(to, "dim_cli::ui") || into(LINE_CLIENT)) {
        return Some("shared modules serve both clients");
    }
    if (within(from, "dim_cli::ui") || within(from, "dim_cli::session_host")) && into(LINE_CLIENT) {
        return Some("the terminal UI does not use the line client");
    }
    if within(from, "dim_cli::session_host") && within(to, "dim_cli::ui") {
        return Some("the session host does not use the UI");
    }
    if within(from, "dim_cli::ui::text") && !within(to, "dim_cli::ui::text") {
        return Some("text measurement is a leaf");
    }
    if within(from, "dim_cli::ui::render")
        && within(to, "dim_cli::ui")
        && ![
            "dim_cli::ui::render",
            "dim_cli::ui::text",
            "dim_cli::ui::theme",
            "dim_cli::ui::terminal",
            "dim_cli::ui::i18n",
        ]
        .iter()
        .any(|root| within(to, root))
    {
        return Some("rendering does not know the transcript or the panels");
    }
    if within(to, "dim_cli::ui::tui")
        && within(from, "dim_cli::ui")
        && !within(from, "dim_cli::ui::tui")
    {
        return Some("parts of the UI do not reach up into the App");
    }
    if within(to, "dim_cli::ui::composer")
        && within(from, "dim_cli::ui")
        && !within(from, "dim_cli::ui::composer")
        && !within(from, "dim_cli::ui::tui")
    {
        return Some("only the App drives the composer");
    }
    None
}
fn flatten_import(tree: &UseTree, mut prefix: Vec<String>, imports: &mut Vec<Import>) {
    match tree {
        UseTree::Path(path) => {
            prefix.push(path.ident.to_string());
            flatten_import(&path.tree, prefix, imports);
        }
        UseTree::Name(name) => {
            let alias = if name.ident == "self" {
                prefix.last().cloned()
            } else {
                prefix.push(name.ident.to_string());
                Some(name.ident.to_string())
            };
            imports.push(Import {
                path: prefix,
                alias,
                glob: false,
            });
        }
        UseTree::Rename(rename) => {
            if rename.ident != "self" {
                prefix.push(rename.ident.to_string());
            }
            imports.push(Import {
                path: prefix,
                alias: Some(rename.rename.to_string()),
                glob: false,
            });
        }
        UseTree::Glob(_) => imports.push(Import {
            path: prefix,
            alias: None,
            glob: true,
        }),
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_import(item, prefix.clone(), imports);
            }
        }
    }
}
fn item_name(item: &Item) -> Option<String> {
    let name = match item {
        Item::Const(i) => &i.ident,
        Item::Enum(i) => &i.ident,
        Item::Fn(i) => &i.sig.ident,
        Item::Static(i) => &i.ident,
        Item::Struct(i) => &i.ident,
        Item::Trait(i) => &i.ident,
        Item::TraitAlias(i) => &i.ident,
        Item::Type(i) => &i.ident,
        Item::Union(i) => &i.ident,
        _ => return None,
    };
    Some(name.to_string())
}
#[derive(Default)]
struct Paths {
    paths: BTreeSet<Vec<String>>,
    macros: BTreeSet<String>,
    business_document: bool,
}
impl<'ast> Visit<'ast> for Paths {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        self.business_document |= node.trait_.as_ref().is_some_and(|(_, path, _)| {
            path.segments
                .last()
                .is_some_and(|segment| segment.ident == "BusinessDocument")
        });
        syn::visit::visit_item_impl(self, node);
    }
    fn visit_visibility(&mut self, _: &'ast syn::Visibility) {}
    fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {}
    fn visit_item_use(&mut self, _: &'ast syn::ItemUse) {}
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.macros.insert(
            node.path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        );
        self.visit_path(&node.path);
    }
    fn visit_path(&mut self, path: &'ast syn::Path) {
        self.paths.insert(
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect(),
        );
        syn::visit::visit_path(self, path);
    }
}
fn escape(label: &str) -> String {
    label
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_workspace(prefix: &str) -> PathBuf {
        let workspace = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(workspace.join("src")).unwrap();
        workspace
    }

    #[test]
    fn cli_layers_reject_upward_and_cross_client_dependencies() {
        for (from, to) in [
            ("dim_cli::mcp", "dim_cli::display"),
            ("dim_cli::attachments", "dim_cli::ui::composer"),
            ("dim_cli::ui::tui", "dim_cli::composer"),
            ("dim_cli::session_host", "dim_cli::ui::composer"),
            ("dim_cli::ui::text", "dim_cli::ui::theme"),
            (
                "dim_cli::ui::render::panel",
                "dim_cli::ui::transcript::blocks",
            ),
            ("dim_cli::ui::approvals_panel", "dim_cli::ui::tui"),
            ("dim_cli::ui::list_panel", "dim_cli::ui::composer::palette"),
        ] {
            assert!(cli_layer_violation(from, to).is_some(), "{from} -> {to}");
        }
        for (from, to) in [
            ("dim_cli::ui::tui", "dim_cli::ui::composer"),
            ("dim_cli::ui::transcript::stream", "dim_cli::ui::render"),
            ("dim_cli::ui::render::panel", "dim_cli::ui::text"),
            ("dim_cli::session_host", "dim_cli::mcp"),
            ("dim_cli::mcp_command", "dim_cli::input"),
            ("dim_cli::ui::composer", "dim_cli::attachments"),
        ] {
            assert!(cli_layer_violation(from, to).is_none(), "{from} -> {to}");
        }
    }

    #[test]
    fn business_boundaries_follow_source_aliases_and_preserve_builtin_contracts() {
        let workspace = std::env::temp_dir().join(format!(
            "dim-module-boundaries-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"dim-app-server\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n").unwrap();
        let definitions = r#"
            mod mcp { pub struct Config; pub mod management { pub struct Request; } }
            mod storage { pub struct Database; }
            mod provider { pub struct ModelPreferences; }
            mod aliases { pub use crate::mcp::{Config as Document, management::Request}; }
        "#;
        for (source, expected) in [
            (
                "mod settings { pub type Db = crate::storage::Database; }
                 mod protocol { pub type Model = crate::provider::ModelPreferences; }
                 mod connection { pub type Mcp = crate::mcp::Config; }",
                None,
            ),
            (
                "mod settings { mod native { pub type Document = crate::aliases::Document; } }",
                Some("settings business dependency"),
            ),
            (
                "mod hooks { pub struct Rule; }
                 mod settings { pub type Document = crate::hooks::Rule; }",
                Some("settings business dependency"),
            ),
            (
                "mod settings {
                     pub trait BusinessDocument {}
                     mod native { struct Approvals; impl super::BusinessDocument for Approvals {} }
                 }",
                Some("settings business document"),
            ),
            (
                "mod settings { pub trait BusinessDocument {} }
                 mod hooks { struct Rules; impl crate::settings::BusinessDocument for Rules {} }",
                None,
            ),
            (
                "mod protocol { use crate::aliases::*; pub fn decode(_: Request) {} }",
                Some("protocol management dependency"),
            ),
            (
                "mod skills_management { pub struct Request; }
                 mod protocol { pub type Request = crate::skills_management::Request; }",
                Some("protocol management dependency"),
            ),
        ] {
            fs::write(
                workspace.join("src/lib.rs"),
                format!("{definitions}\n{source}"),
            )
            .unwrap();
            let result = generate(&workspace);
            match expected {
                None => drop(result.unwrap()),
                Some(expected) => assert!(result.unwrap_err().to_string().contains(expected)),
            }
        }
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn a_package_binary_resolves_its_library_through_the_crate_name() {
        let workspace = temporary_workspace("dim-module-bin");
        fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"dim-agent-demo\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[lib]\npath = \"src/lib.rs\"\n[[bin]]\nname = \"demo\"\npath = \"src/main.rs\"\n[workspace]\n").unwrap();
        fs::write(
            workspace.join("src/lib.rs"),
            "pub mod thing { pub struct Value; }\n",
        )
        .unwrap();
        fs::write(
            workspace.join("src/main.rs"),
            "use dim_agent_demo::thing::Value;\nfn main() { let _: Option<Value> = None; }\n",
        )
        .unwrap();
        let output = generate(&workspace).unwrap();
        let json: Value = serde_json::from_str(
            &output
                .iter()
                .find(|(name, _)| *name == "modules.json")
                .unwrap()
                .1,
        )
        .unwrap();
        assert!(
            json["edges"]
                .as_array()
                .unwrap()
                .iter()
                .any(|edge| edge["from"] == "dim_agent_demo::@bin:demo"
                    && edge["to"] == "dim_agent_demo::thing"),
            "a binary must resolve its library through the crate name"
        );
        fs::remove_dir_all(workspace).unwrap();
    }

    #[test]
    fn cyclic_crate_globs_terminate_without_adding_unused_edges() {
        let workspace = temporary_workspace("dim-module-glob");
        fs::write(workspace.join("Cargo.toml"), "[package]\nname = \"dim-agent-demo\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[workspace]\n").unwrap();
        fs::write(
            workspace.join("src/lib.rs"),
            "mod a { use crate::b::*; pub struct A; }\nmod b { use crate::a::*; pub struct B; }\nmod c { use crate::a::A; pub fn read(_: A) {} }\n",
        )
        .unwrap();
        let output = generate(&workspace).unwrap();
        let json: Value = serde_json::from_str(
            &output
                .iter()
                .find(|(name, _)| *name == "modules.json")
                .unwrap()
                .1,
        )
        .unwrap();
        let edges = json["edges"].as_array().unwrap();
        assert!(
            !edges.iter().any(|edge| matches!(
                (edge["from"].as_str(), edge["to"].as_str()),
                (Some("dim_agent_demo::a"), Some("dim_agent_demo::b"))
                    | (Some("dim_agent_demo::b"), Some("dim_agent_demo::a"))
            )),
            "an unused glob must not add an edge"
        );
        assert!(
            edges.iter().any(
                |edge| edge["from"] == "dim_agent_demo::c" && edge["to"] == "dim_agent_demo::a"
            ),
            "an actual use creates the edge"
        );
        fs::remove_dir_all(workspace).unwrap();
    }
}
