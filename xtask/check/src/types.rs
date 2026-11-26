//! `types` writes the TypeScript declarations of the services marked
//! `#[rutis::service]` that `dim-app-server` serves (rutis usage §3.4) to
//! `npm/branch-services.d.ts`, and the addresses a plugin of any language uses
//! to call a Session's or a line's data face by id
//! ([#2127](https://github.com/arcships/dim/issues/2127)); `check` fails when
//! that file is stale.
use std::path::{Path, PathBuf};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn file(root: &Path) -> PathBuf {
    root.join("npm/branch-services.d.ts")
}

/// The two data operations' requests, narrowed to what a route resolves: the
/// `service` is a data face of the target scope, `method` one of its methods,
/// and `args` the method's parameters, so a typo fails `tsc`
/// ([#2127](https://github.com/arcships/dim/issues/2127)).
fn calls(root: &Path, services: &str) -> Result<String> {
    let unroutable = unroutable(root)?;
    let declared: Vec<&str> = services
        .lines()
        .filter_map(|line| {
            line.strip_prefix("export interface ")
                .or_else(|| line.strip_prefix("export type "))
                .and_then(|rest| rest.split([' ', '<', '{', '=']).next())
        })
        .collect();
    let mut session = Vec::new();
    let mut line = Vec::new();
    for (name, group) in registered(root)? {
        // The Session's own scope resolves a line and the Session's faces; it
        // is not one of the faces a routed call resolves.
        if unroutable.contains(&name) || name == "sessionScope" {
            continue;
        }
        let Some(ty) = declared.iter().find(|ty| catalog_name(ty) == name).copied() else {
            continue;
        };
        match group.as_str() {
            "session" => session.push((name, ty.to_owned())),
            "branch" => line.push((name, ty.to_owned())),
            _ => {}
        }
    }
    session.sort();
    session.dedup();
    line.sort();
    line.dedup();
    let refused = lifecycle(root)?;
    // The list and the signatures must name the same operations: a signature
    // that cannot cross languages but is missing fails, and so does a listed
    // operation whose signature can.
    let groups = registered(root)?;
    let derived: Vec<(String, String)> = derived_lifecycle(root)?
        .into_iter()
        // Only a service a route resolves, and only an operation the file
        // declares, is part of that surface.
        .filter(|(service, method)| {
            // Only a Session or branch face crosses through a route at all.
            let in_scope = groups
                .iter()
                .any(|(name, group)| name == service && (group == "session" || group == "branch"));
            in_scope
                && !unroutable.contains(service)
                && service != "sessionScope"
                && declares_method(services, service, method)
        })
        .collect();
    let missing: Vec<String> = derived
        .iter()
        .filter(|pair| !refused.contains(pair))
        .map(|(service, method)| format!("{service}.{method}"))
        .collect();
    let extra: Vec<String> = refused
        .iter()
        .filter(|pair| !derived.contains(pair))
        .map(|(service, method)| format!("{service}.{method}"))
        .collect();
    if !missing.is_empty() || !extra.is_empty() {
        return Err(format!(
            "crates/dim-agent-runtime/src/route.rs: LIFECYCLE_METHODS does not match the service signatures; not listed: [{}], listed but routable: [{}]",
            missing.join(", "),
            extra.join(", ")
        )
        .into());
    }
    let faces = |items: &[(String, String)], name: &str| {
        let mut text = format!("export type {name} = {{\n");
        for (member, ty) in items {
            let omitted: Vec<String> = refused
                .iter()
                .filter(|(service, _)| service == member)
                .map(|(_, method)| format!("'{method}'"))
                .collect();
            if omitted.is_empty() {
                text += &format!("  {member}: {ty}\n");
            } else {
                text += &format!("  {member}: Omit<{ty}, {}>\n", omitted.join(" | "));
            }
        }
        text + "}\n"
    };
    let mut text = String::from("/** The data faces a line reaches. */\n");
    text += &faces(&line, "LineFaces");
    text += "\n/** The data faces a Session reaches. */\n";
    text += &faces(&session, "SessionFaces");
    text += "\n/** One call a route resolves: `service` and `method` name a data face and one of its methods, and `args` are that method's parameters. */\ntype Call<Faces, Address> = {\n  [S in keyof Faces]: {\n    [M in keyof Faces[S]]: Address & {\n      service: S\n      method: M\n      args: Faces[S][M] extends (...args: infer A) => unknown ? A : never\n    }\n  }[keyof Faces[S]]\n}[keyof Faces]\n";
    text += "\n/** A call to one of the caller's own Session's lines. */\nexport type LineCall = Call<LineFaces, { lineId: string; sessionId?: never }>\n";
    text += "\n/** A call to one of the caller's own Session's data faces. */\nexport type SessionCall = Call<SessionFaces, { lineId?: never; sessionId?: never }>\n";
    text += "\n/** A call through Agents: a Session's face, or one of its lines' faces. A Session\n * face takes no `lineId`. */\nexport type SessionsCall =\n  | Call<LineFaces, { sessionId: string; lineId: string }>\n  | Call<SessionFaces, { sessionId: string; lineId?: never }>\n";
    text += "\n/** The reply of one call: the target method's own return type. */\nexport type ReplyOf<C> = C extends { service: infer S; method: infer M }\n  ? S extends keyof (LineFaces & SessionFaces)\n    ? M extends keyof (LineFaces & SessionFaces)[S]\n      ? Awaited<\n          (LineFaces & SessionFaces)[S][M] extends (...args: never[]) => infer R ? R : never\n        >\n      : never\n    : never\n  : never\n";
    text += "\n/** What a routed call answers: a method that replies nothing reaches the caller as `null`, while a direct call to it returns `undefined`. */\nexport type RoutedReply<C> = ReplyOf<C> extends void ? null : ReplyOf<C>\n";
    text += "\n/** A Session's own faces, typed per operation: `service` and `method` choose the reply. */\nexport interface SessionScope {\n  call<C extends SessionCall>(call: C): Promise<RoutedReply<C>>\n  lineCall<C extends LineCall>(call: C): Promise<RoutedReply<C>>\n}\n";
    Ok(text)
}

/// The catalog name and group of every service the workspaces' crates
/// register, read from the registration calls themselves: a name is in a group
/// because some `register`/`register_shared_instance` call puts it there.
fn registered(root: &Path) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))? {
        let directory = entry?.path().join("src");
        if !directory.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        sources(&mut files, &directory)?;
        for file in files {
            let text = std::fs::read_to_string(&file)?;
            let ast = syn::parse_file(&text)?;
            registrations(&ast.items, &mut found);
        }
    }
    Ok(found)
}

fn sources(files: &mut Vec<PathBuf>, directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            sources(files, &path)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

fn registrations(items: &[syn::Item], found: &mut Vec<(String, String)>) {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    registrations(items, found);
                }
            }
            syn::Item::Impl(block) => {
                for item in &block.items {
                    if let syn::ImplItem::Fn(function) = item {
                        expressions(&function.block, found);
                    }
                }
            }
            syn::Item::Fn(function) => expressions(&function.block, found),
            _ => {}
        }
    }
}

fn expressions(block: &syn::Block, found: &mut Vec<(String, String)>) {
    for statement in &block.stmts {
        match statement {
            syn::Stmt::Expr(expression, _) => call(expression, found),
            syn::Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    call(&init.expr, found);
                }
            }
            _ => {}
        }
    }
}

/// One `register`/`register_shared_instance` call, with the group its
/// arguments name.
fn call(expression: &syn::Expr, found: &mut Vec<(String, String)>) {
    if let syn::Expr::MethodCall(method) = expression {
        if method.method == "register_shared_instance" {
            let mut args = method.args.iter();
            let Some(name) = args.next().and_then(name_of) else {
                return;
            };
            let Some(group) = args.next().and_then(group_of) else {
                return;
            };
            found.push((name, group.to_owned()));
        }
        return;
    }
    let syn::Expr::Call(call) = expression else {
        return;
    };
    let syn::Expr::Path(path) = &*call.func else {
        return;
    };
    let Some(last) = path.path.segments.last() else {
        return;
    };
    match last.ident.to_string().as_str() {
        "register" | "register_instance" | "register_keyed" => {
            let Some(group) = call.args.iter().find_map(group_of) else {
                return;
            };
            if let Some(ty) = type_argument(&last.arguments) {
                found.push((catalog_name(&ty), group.to_owned()));
            }
        }
        "register_shared_instance" => {
            let mut args = call.args.iter();
            let Some(name) = args.next().and_then(name_of) else {
                return;
            };
            let Some(group) = args.next().and_then(group_of) else {
                return;
            };
            found.push((name, group.to_owned()));
        }
        _ => {}
    }
}

/// The role a group argument names: `session`, `branch`, or their constants.
fn group_of(expression: &syn::Expr) -> Option<&'static str> {
    match expression {
        syn::Expr::Path(path) => {
            let ident = path.path.segments.last()?.ident.to_string();
            match ident.as_str() {
                "session" => Some("session"),
                "branch" => Some("branch"),
                "SESSION_GROUP" => Some("session"),
                "BRANCH_GROUP" => Some("branch"),
                _ => None,
            }
        }
        syn::Expr::Call(call) => call.args.iter().find_map(group_of),
        syn::Expr::Reference(reference) => group_of(&reference.expr),
        _ => None,
    }
}

/// The Rust type a registration is for: `register::<T>`'s argument, or a
/// string name.
fn type_argument(arguments: &syn::PathArguments) -> Option<String> {
    Some(catalog_name(&rust_type_argument(arguments)?))
}

/// The Rust type a turbofish names, as written.
fn rust_type_argument(arguments: &syn::PathArguments) -> Option<String> {
    let syn::PathArguments::AngleBracketed(arguments) = arguments else {
        return None;
    };
    arguments.args.iter().find_map(|argument| {
        let syn::GenericArgument::Type(ty) = argument else {
            return None;
        };
        Some(match ty {
            syn::Type::Path(path) => path.path.segments.last()?.ident.to_string(),
            syn::Type::TraitObject(object) => object.bounds.iter().find_map(|bound| {
                let syn::TypeParamBound::Trait(bound) = bound else {
                    return None;
                };
                Some(bound.path.segments.last()?.ident.to_string())
            })?,
            _ => return None,
        })
    })
}

/// A name argument as written: `"name"` or `name_of::<T>()`.
fn name_of(expression: &syn::Expr) -> Option<String> {
    match expression {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(name),
            ..
        }) => Some(name.value()),
        syn::Expr::Call(call) => {
            let syn::Expr::Path(path) = &*call.func else {
                return None;
            };
            let last = path.path.segments.last()?;
            if last.ident != "name_of" {
                return None;
            }
            type_argument(&last.arguments)
        }
        syn::Expr::Path(_) => None,
        syn::Expr::Reference(reference) => name_of(&reference.expr),
        syn::Expr::Macro(mac) => {
            let tokens = mac.mac.tokens.to_string();
            tokens.split('<').nth(1).map(|rest| {
                catalog_name(
                    rest.trim_end_matches('>')
                        .trim()
                        .rsplit("::")
                        .next()
                        .unwrap_or(""),
                )
            })
        }
        _ => None,
    }
}

/// The operations routing does not resolve, read from the runtime's own list:
/// a call that needs a cancellation or a caller-side cleanup keeps its lifetime
/// only when it is made directly.
fn lifecycle(root: &Path) -> Result<Vec<(String, String)>> {
    let path = root.join("crates/dim-agent-runtime/src/route.rs");
    let ast = syn::parse_file(&std::fs::read_to_string(&path)?)?;
    let mut found = Vec::new();
    let mut seen = false;
    for item in &ast.items {
        let syn::Item::Const(declaration) = item else {
            continue;
        };
        if declaration.ident != "LIFECYCLE_METHODS" {
            continue;
        }
        let syn::Expr::Array(array) = &*declaration.expr else {
            return Err(format!("{}: LIFECYCLE_METHODS is not an array", path.display()).into());
        };
        for element in &array.elems {
            let syn::Expr::Tuple(tuple) = element else {
                return Err(format!(
                    "{}: a LIFECYCLE_METHODS entry is not a pair",
                    path.display()
                )
                .into());
            };
            let mut parts = tuple.elems.iter().filter_map(|part| match part {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(text),
                    ..
                }) => Some(text.value()),
                _ => None,
            });
            match (parts.next(), parts.next()) {
                (Some(service), Some(method)) => found.push((service, method)),
                _ => {
                    return Err(format!(
                        "{}: a LIFECYCLE_METHODS entry is not a pair of names",
                        path.display()
                    )
                    .into());
                }
            }
        }
        seen = true;
    }
    if !seen {
        return Err(format!("{}: LIFECYCLE_METHODS is missing", path.display()).into());
    }
    Ok(found)
}

/// The operations a catalog's own method signatures say a routed call cannot
/// carry: a parameter that is the calling row, a cancellation, or a callback.
/// Read from the `#[rutis::service]` blocks themselves, so a new operation of
/// that kind cannot slip past the list the runtime refuses.
///
/// What it recognizes: a `Ctx` or a reference to one, the `CancelledFn` and
/// `CancellationToken` aliases, and `Fn(..)` as a trait object, behind `Arc`, or
/// as `impl Fn`, and a bare `fn(..)` pointer. What it still misses: any other
/// alias of those types; an operation written that way is a review-time
/// guarantee, and the two-way comparison reports it as a listed operation whose
/// signature looks routable.
fn derived_lifecycle(root: &Path) -> Result<Vec<(String, String)>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))? {
        let directory = entry?.path().join("src");
        if !directory.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        sources(&mut files, &directory)?;
        for file in files {
            let ast = syn::parse_file(&std::fs::read_to_string(&file)?)?;
            service_blocks(&ast.items, &mut found);
        }
    }
    found.sort();
    found.dedup();
    Ok(found)
}

fn service_blocks(items: &[syn::Item], found: &mut Vec<(String, String)>) {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    service_blocks(items, found);
                }
            }
            syn::Item::Impl(block) if block.attrs.iter().any(is_service) => {
                let service = type_name(&block.self_ty);
                for item in &block.items {
                    let syn::ImplItem::Fn(function) = item else {
                        continue;
                    };
                    if function
                        .attrs
                        .iter()
                        .any(|attr| is_service(attr) && skip_argument(attr))
                    {
                        continue;
                    }
                    let carried = function.sig.inputs.iter().any(|input| {
                        let syn::FnArg::Typed(typed) = input else {
                            return false;
                        };
                        carries_lifecycle(&typed.ty)
                    });
                    if carried {
                        found.push((
                            catalog_name(&service),
                            camel(&function.sig.ident.to_string()),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

/// Whether a parameter type is one a routed call cannot carry: the calling
/// row, a cancellation, or a callback.
fn carries_lifecycle(ty: &syn::Type) -> bool {
    match ty {
        syn::Type::Reference(reference) => carries_lifecycle(&reference.elem),
        syn::Type::Path(path) => {
            let Some(last) = path.path.segments.last() else {
                return false;
            };
            matches!(
                last.ident.to_string().as_str(),
                "Ctx" | "CancelledFn" | "CancellationToken"
            ) || match &last.arguments {
                syn::PathArguments::AngleBracketed(arguments) => {
                    arguments.args.iter().any(|argument| match argument {
                        syn::GenericArgument::Type(ty) => carries_lifecycle(ty),
                        _ => false,
                    })
                }
                _ => false,
            }
        }
        syn::Type::ImplTrait(implemented) => implemented.bounds.iter().any(|bound| {
            matches!(bound, syn::TypeParamBound::Trait(bound)
                if bound.path.segments.last().is_some_and(|segment| segment.ident == "Fn"))
        }),
        syn::Type::BareFn(_) => true,
        syn::Type::TraitObject(object) => object.bounds.iter().any(|bound| {
            matches!(bound, syn::TypeParamBound::Trait(bound)
                if bound.path.segments.last().is_some_and(|segment| segment.ident == "Fn"))
        }),
        _ => false,
    }
}

/// Whether the file declares `method` on `service`'s interface.
fn declares_method(services: &str, service: &str, method: &str) -> bool {
    let typed = services
        .lines()
        .filter_map(|line| {
            line.strip_prefix("export interface ")
                .or_else(|| line.strip_prefix("export type "))
                .and_then(|rest| rest.split([' ', '<', '{', '=']).next())
        })
        .find(|ty| catalog_name(ty) == service);
    let Some(typed) = typed else {
        return false;
    };
    let start = match services.find(&format!("export interface {typed} {{")) {
        Some(start) => start,
        None => return false,
    };
    services[start..]
        .lines()
        .take_while(|line| *line != "}")
        .any(|line| line.trim_start().starts_with(&format!("{method}(")))
}

/// `#[service]`, `#[service(skip)]` or their `rutis::` spellings.
fn is_service(attr: &syn::Attribute) -> bool {
    let path = attr.path();
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect();
    matches!(segments.as_slice(), [service] if service == "service")
        || matches!(segments.as_slice(), [root, service] if root == "rutis" && service == "service")
}

fn skip_argument(attr: &syn::Attribute) -> bool {
    attr.parse_args::<syn::Ident>()
        .is_ok_and(|argument| argument == "skip")
}

/// The last path segment of a type.
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

/// The name a method crosses as: `publish_file` is `publishFile`.
fn camel(ident: &str) -> String {
    let mut camel = String::new();
    for (index, part) in ident.split('_').enumerate() {
        if index == 0 {
            camel += part;
            continue;
        }
        let mut chars = part.chars();
        if let Some(first) = chars.next() {
            camel += &first.to_uppercase().collect::<String>();
            camel += chars.as_str();
        }
    }
    camel
}

/// The names routing does not resolve, read from the runtime that refuses
/// them: its registration points, plus the two lifecycle services. A missing
/// list is an error, never an empty one.
fn unroutable(root: &Path) -> Result<Vec<String>> {
    let path = root.join("crates/dim-agent-runtime/src/route.rs");
    let source = std::fs::read_to_string(&path)?;
    let ast = syn::parse_file(&source).map_err(|error| format!("{}: {error}", path.display()))?;
    let mut names = vec!["sessionLifecycle".to_owned(), "branchLifecycle".to_owned()];
    let mut found = false;
    for item in &ast.items {
        let syn::Item::Const(declaration) = item else {
            continue;
        };
        if declaration.ident != "REGISTRATION_POINTS" {
            continue;
        }
        let syn::Expr::Array(array) = &*declaration.expr else {
            return Err(format!("{}: REGISTRATION_POINTS is not an array", path.display()).into());
        };
        for element in &array.elems {
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(name),
                ..
            }) = element
            else {
                return Err(format!(
                    "{}: REGISTRATION_POINTS holds an entry that is not a string",
                    path.display()
                )
                .into());
            };
            names.push(name.value());
        }
        found = true;
    }
    if !found {
        return Err(format!("{}: REGISTRATION_POINTS is missing", path.display()).into());
    }
    names.extend(internal(root)?);
    Ok(names)
}

/// The names an assembly marks internal, read from the calls that mark them and
/// from the product's table of `(name, reason)` pairs: a route refuses them, so
/// the generated requests do not offer them.
fn internal(root: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(root.join("crates"))? {
        let directory = entry?.path().join("src");
        if !directory.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        sources(&mut files, &directory)?;
        for file in files {
            let ast = syn::parse_file(&std::fs::read_to_string(&file)?)?;
            marks(&ast.items, &mut names);
            internal_table(&ast.items, &mut names);
        }
    }
    Ok(names)
}

fn marks(items: &[syn::Item], names: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    marks(items, names);
                }
            }
            syn::Item::Fn(function) => marks_in(&function.block, names),
            syn::Item::Impl(block) => {
                for item in &block.items {
                    if let syn::ImplItem::Fn(function) = item {
                        marks_in(&function.block, names);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The product's table of internal names: `for (name, reason) in [("a", "r"), …]`
/// feeding `internal_name(name, reason)`.
fn internal_table(items: &[syn::Item], names: &mut Vec<String>) {
    for item in items {
        match item {
            syn::Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    internal_table(items, names);
                }
            }
            syn::Item::Fn(function) => tables_in(&function.block, names),
            _ => {}
        }
    }
}

fn tables_in(block: &syn::Block, names: &mut Vec<String>) {
    for statement in &block.stmts {
        let syn::Stmt::Expr(expression, _) = statement else {
            continue;
        };
        let syn::Expr::ForLoop(loop_) = expression else {
            continue;
        };
        let syn::Expr::Array(array) = &*loop_.expr else {
            continue;
        };
        if !feeds_internal_name(&loop_.body) {
            continue;
        }
        for element in &array.elems {
            let syn::Expr::Tuple(tuple) = element else {
                continue;
            };
            if let Some(name) = tuple.elems.first().and_then(name_of) {
                names.push(name);
            }
        }
    }
}

/// Whether a loop body hands its pair to `internal_name`, as the product's
/// face table does; any other table of pairs is unrelated data.
fn feeds_internal_name(block: &syn::Block) -> bool {
    fn mentions(expression: &syn::Expr) -> bool {
        match expression {
            syn::Expr::Call(call) => {
                let named = match &*call.func {
                    syn::Expr::Path(path) => path
                        .path
                        .segments
                        .last()
                        .is_some_and(|segment| segment.ident == "internal_name"),
                    _ => false,
                };
                named || call.args.iter().any(mentions)
            }
            syn::Expr::MethodCall(call) => {
                call.method == "internal_name" || call.args.iter().any(mentions)
            }
            _ => false,
        }
    }
    block.stmts.iter().any(|statement| match statement {
        syn::Stmt::Expr(expression, _) => mentions(expression),
        syn::Stmt::Local(local) => local.init.as_ref().is_some_and(|init| mentions(&init.expr)),
        _ => false,
    })
}

fn marks_in(block: &syn::Block, names: &mut Vec<String>) {
    for statement in &block.stmts {
        let expression = match statement {
            syn::Stmt::Expr(expression, _) => expression,
            syn::Stmt::Local(local) => match &local.init {
                Some(init) => &init.expr,
                None => continue,
            },
            _ => continue,
        };
        let syn::Expr::Call(call) = expression else {
            continue;
        };
        let syn::Expr::Path(path) = &*call.func else {
            continue;
        };
        let Some(last) = path.path.segments.last() else {
            continue;
        };
        let mut args = call.args.iter();
        match last.ident.to_string().as_str() {
            "internal_name" => {
                if let Some(name) = args.find_map(name_of) {
                    names.push(name);
                }
            }
            "internal" => {
                if let Some(name) = type_argument(&last.arguments) {
                    names.push(catalog_name(&name));
                }
            }
            _ => {}
        }
    }
}

/// The catalog name of a Rust type: its type name with the leading word
/// lowercased, as `#[rutis::service]` names a type (`InputQueue` is
/// `inputQueue`).
fn catalog_name(type_name: &str) -> String {
    let ident = type_name
        .trim_start_matches("dyn ")
        .split(['<', ' '])
        .next()
        .unwrap_or(type_name);
    let ident = ident.rsplit("::").next().unwrap_or(ident);
    let chars: Vec<char> = ident.chars().collect();
    let upper = chars.iter().take_while(|c| c.is_uppercase()).count();
    let lower = if upper > 1 && upper < chars.len() && chars[upper].is_lowercase() {
        upper - 1
    } else {
        upper.max(1).min(chars.len())
    };
    chars
        .iter()
        .enumerate()
        .flat_map(|(index, c)| {
            if index < lower {
                c.to_lowercase().collect::<Vec<_>>()
            } else {
                vec![*c]
            }
        })
        .collect()
}

fn declarations(root: &Path) -> Result<String> {
    let services = rutis_types::Exports::new()
        .declared("serde_json::Value", "import('./plugin.js').BridgeJsonValue")
        .declared("serde_json::Map", "import('./plugin.js').BridgeJsonObject")
        .declared(
            "dim_agent_types::PermissionPolicy",
            "import('./plugin.js').PermissionPolicy",
        )
        .declared(
            "dim_agent_types::AgentEvent",
            "import('./plugin.js').BranchEvent",
        )
        .declared(
            "dim_agent_types::EventDraft",
            "{ type: string; name: string; data: unknown; tags?: string[]; context_render?: boolean; context_state?: boolean }",
        )
        .declared(
            "dim_agent_runtime::TimelineIdentity",
            "import('./plugin.js').SessionRef",
        )
        .declared(
            "dim_agent_runtime::SessionIdentity",
            "{ sessionId: string; restored: boolean }",
        )
        .declared(
            "dim_agent_runtime::SessionConfig",
            "import('./plugin.js').SessionConfig",
        )
        .declared(
            "dim_agent_types::ModelConfig",
            "import('./plugin.js').ModelConfig",
        )
        .declared("std::num::NonZeroU64", "number | bigint")
        .declared(
            "dim_agent_types::ToolCall",
            "{ id: string; name: string; input: { [key: string]: unknown } }",
        )
        .declared(
            "dim_agent_types::ToolResult",
            "import('./plugin.js').ToolResult",
        )
        .declared(
            "dim_agent_types::TimelinePosition",
            "{ branch_id: string; sequence: number | bigint }",
        )
        .declarations(root.join("crates/dim-app-server"))?;
    let calls = calls(root, &services)?;
    let mut text = replaced(services);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    Ok(text + "\n" + &calls)
}

/// Drops the generated `LineCall`/`SessionCall` declarations, which the
/// narrowed ones replace.
fn replaced(services: String) -> String {
    let mut text = String::new();
    let mut skipping = false;
    for line in services.lines() {
        if line.starts_with("export type LineCall = {")
            || line.starts_with("export type SessionCall = {")
        {
            skipping = true;
            continue;
        }
        if skipping {
            if line == "}" {
                skipping = false;
            }
            continue;
        }
        if line == "/** A call to one of the caller's own Session's lines. */"
            || line == "/** A call to one of the caller's own Session's data faces. */"
        {
            continue;
        }
        text += line;
        text.push('\n');
    }
    text
}

pub fn write(root: &Path) -> Result<()> {
    std::fs::write(file(root), declarations(root)?)?;
    Ok(())
}

pub fn check(root: &Path) -> Result<()> {
    let path = file(root);
    let written = std::fs::read_to_string(&path).unwrap_or_default();
    if written != declarations(root)? {
        return Err(format!(
            "{}: service declarations are stale; run `cargo run -p xtask -- types`",
            path.display()
        )
        .into());
    }
    println!("service declarations passed");
    Ok(())
}

#[cfg(test)]
mod derivation_tests {
    use super::*;

    fn tree(source: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "dim-derived-{}-{}",
            std::process::id(),
            source.len()
        ));
        std::fs::create_dir_all(root.join("crates/example/src")).unwrap();
        std::fs::write(root.join("crates/example/src/lib.rs"), source).unwrap();
        root
    }

    #[test]
    fn a_callback_operation_is_derived_and_a_plain_one_is_not() {
        let root = tree(
            r#"
            pub struct Example;
            #[rutis::service]
            impl Example {
                /// A plain read.
                pub async fn get(&self) -> Result<u64, Error> { Ok(1) }
                /// Takes the calling row.
                pub async fn read(&self, caller: &Ctx, path: String) -> Result<u64, Error> { Ok(1) }
                /// Takes a cancellation.
                pub async fn wait(&self, cancelled: CancelledFn) -> Result<u64, Error> { Ok(1) }
            }
            "#,
        );
        let derived = derived_lifecycle(&root).expect("the derivation reads the tree");
        assert_eq!(
            derived,
            vec![
                ("example".to_owned(), "read".to_owned()),
                ("example".to_owned(), "wait".to_owned()),
            ],
            "{derived:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_comparison_names_the_operation_that_disagrees() {
        let derived = [
            ("example".to_owned(), "read".to_owned()),
            ("example".to_owned(), "wait".to_owned()),
        ];
        let listed = [("example".to_owned(), "read".to_owned())];
        let missing: Vec<String> = derived
            .iter()
            .filter(|pair| !listed.contains(pair))
            .map(|(service, method)| format!("{service}.{method}"))
            .collect();
        assert_eq!(missing, vec!["example.wait".to_owned()]);
        let listed = [
            ("example".to_owned(), "get".to_owned()),
            ("example".to_owned(), "read".to_owned()),
            ("example".to_owned(), "wait".to_owned()),
        ];
        let extra: Vec<String> = listed
            .iter()
            .filter(|pair| !derived.contains(pair))
            .map(|(service, method)| format!("{service}.{method}"))
            .collect();
        assert_eq!(extra, vec!["example.get".to_owned()]);
    }
}
