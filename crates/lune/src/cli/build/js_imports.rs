use std::collections::{BTreeMap, BTreeSet};

use full_moon::{
    ast::{
        Call, Expression, Field, FunctionArgs, FunctionCall, Index, LocalAssignment, Prefix,
        Suffix, TableConstructor, Var, luau::ConstAssignment,
    },
    tokenizer::{TokenReference, TokenType},
    visitors::Visitor,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct JsImportDiscovery {
    /// Literal imports paired with the receiving context's `ModuleRoot`, if any.
    pub imports: BTreeSet<(String, Option<String>)>,
    pub has_dynamic_imports: bool,
    /// Literal imports on receivers that could not be traced to a context.
    pub unknown_specifiers: BTreeSet<String>,
}

enum Binding {
    Alias(String),
    NewContext(Option<String>),
}

#[derive(Default)]
struct ImportVisitor {
    bindings: Vec<(Vec<String>, Binding)>,
    imports: Vec<ImportCall>,
}

struct ImportCall {
    receiver: String,
    specifier: Option<String>,
}

impl ImportVisitor {
    fn record_binding<'a>(
        &mut self,
        names: impl Iterator<Item = TokenReference>,
        expressions: impl Iterator<Item = &'a Expression>,
    ) {
        let names = names
            .map(|name| name.token().to_string())
            .collect::<Vec<_>>();
        for expression in expressions {
            if let Some(module_root) = new_context_root(expression) {
                self.bindings
                    .push((names.clone(), Binding::NewContext(module_root)));
                return;
            }
            if let Expression::Var(Var::Name(name)) = expression {
                self.bindings
                    .push((names.clone(), Binding::Alias(name.token().to_string())));
                return;
            }
        }
    }
}

impl Visitor for ImportVisitor {
    fn visit_const_assignment(&mut self, assignment: &ConstAssignment) {
        self.record_binding(
            assignment.names().iter().cloned(),
            assignment.expressions().iter(),
        );
    }

    fn visit_local_assignment(&mut self, assignment: &LocalAssignment) {
        self.record_binding(
            assignment.names().iter().cloned(),
            assignment.expressions().iter(),
        );
    }

    fn visit_function_call(&mut self, call: &FunctionCall) {
        let Prefix::Name(receiver) = call.prefix() else {
            return;
        };
        let receiver = receiver.token().to_string();
        for suffix in call.suffixes() {
            let Suffix::Call(Call::MethodCall(method)) = suffix else {
                continue;
            };
            if method.name().token().to_string() != "Import" {
                continue;
            }
            self.imports.push(ImportCall {
                receiver: receiver.clone(),
                specifier: literal_argument(method.args()),
            });
        }
    }
}

/// Finds literal JavaScript imports on variables created with `newContext`.
///
/// Context provenance is propagated through aliases, and the context's literal
/// `ModuleRoot` is carried so builds resolve modules exactly like runtime execution.
pub fn discover_js_imports(source: &str) -> Result<JsImportDiscovery, Vec<full_moon::Error>> {
    let ast = full_moon::parse(source)?;
    let mut visitor = ImportVisitor::default();
    visitor.visit_ast(&ast);

    let mut contexts = BTreeMap::new();
    for (names, binding) in &visitor.bindings {
        if let Binding::NewContext(module_root) = binding {
            for name in names {
                contexts.insert(name.clone(), module_root.clone());
            }
        }
    }
    // Resolve alias chains after collection so forward references also work.
    loop {
        let mut changed = false;
        for (names, binding) in &visitor.bindings {
            if let Binding::Alias(source) = binding
                && let Some(module_root) = contexts.get(source).cloned()
            {
                for name in names {
                    if contexts.insert(name.clone(), module_root.clone()).is_none() {
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }

    let mut discovery = JsImportDiscovery::default();
    for import in visitor.imports {
        let Some(module_root) = contexts.get(&import.receiver) else {
            if let Some(specifier) = import.specifier {
                discovery.unknown_specifiers.insert(specifier);
            }
            continue;
        };
        match import.specifier {
            Some(specifier) => {
                discovery.imports.insert((specifier, module_root.clone()));
            }
            None => discovery.has_dynamic_imports = true,
        }
    }
    Ok(discovery)
}

/// Returns the literal `ModuleRoot` when the expression creates a context.
fn new_context_root(expression: &Expression) -> Option<Option<String>> {
    let Expression::FunctionCall(call) = expression else {
        return None;
    };
    let suffixes = call.suffixes().collect::<Vec<_>>();
    for pair in suffixes.windows(2) {
        let [
            Suffix::Index(Index::Dot { name, .. }),
            Suffix::Call(Call::AnonymousCall(arguments)),
        ] = pair
        else {
            continue;
        };
        if name.token().to_string() != "newContext" {
            continue;
        }
        return Some(module_root_from_args(arguments));
    }
    None
}

fn module_root_from_args(arguments: &FunctionArgs) -> Option<String> {
    let FunctionArgs::Parentheses { arguments, .. } = arguments else {
        return None;
    };
    let Expression::TableConstructor(table) = arguments.iter().next()? else {
        return None;
    };
    module_root_from_table(table)
}

fn module_root_from_table(table: &TableConstructor) -> Option<String> {
    table.fields().iter().find_map(|field| {
        let Field::NameKey { key, value, .. } = field else {
            return None;
        };
        if key.token().to_string() != "ModuleRoot" {
            return None;
        }
        let Expression::String(token) = value.as_ref() else {
            return None;
        };
        string_literal(token)
    })
}

fn literal_argument(arguments: &FunctionArgs) -> Option<String> {
    match arguments {
        FunctionArgs::String(token) => string_literal(token),
        FunctionArgs::Parentheses { arguments, .. } => {
            let mut arguments = arguments.iter();
            let expression = arguments.next()?;
            if arguments.next().is_some() {
                return None;
            }
            match expression {
                Expression::String(token) => string_literal(token),
                Expression::Parentheses { expression, .. } => match expression.as_ref() {
                    Expression::String(token) => string_literal(token),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn string_literal(token: &TokenReference) -> Option<String> {
    match token.token_type() {
        TokenType::StringLiteral { literal, .. } if !literal.contains('\\') => {
            Some(literal.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specifiers(source: &str) -> Vec<String> {
        discover_js_imports(source)
            .unwrap()
            .imports
            .into_iter()
            .map(|(specifier, _)| specifier)
            .collect()
    }

    #[test]
    fn finds_literal_method_imports_on_contexts() {
        let source = r#"
            const js = require("@lune/js")
            const Context = js.newContext()
            const First = Context:Import("./First.ts")
            const Second = Context:Import './Second.ts'
            const Third = Context:Import [[./Third.ts]]
            const Fourth = Context:Import(("./First.ts"))
        "#;
        assert_eq!(
            specifiers(source),
            vec!["./First.ts", "./Second.ts", "./Third.ts"]
        );
    }

    #[test]
    fn follows_aliases_and_module_roots() {
        let source = r#"
            const Context = js.newContext({ ModuleRoot = "./Source" })
            const Alias = Context
            const Catalog = Alias:Import("/Catalog.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Catalog.ts".to_string(), Some("./Source".to_string()))]
        );
    }

    #[test]
    fn reports_unknown_literal_receivers() {
        let source = r#"
            local Importer = {}
            Importer:Import("asset.json")
            local Context = js.newContext()
            Context:Import("./Known.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("./Known.ts".to_string(), None)]
        );
        assert_eq!(
            discovery.unknown_specifiers.into_iter().collect::<Vec<_>>(),
            vec!["asset.json".to_string()]
        );
    }

    #[test]
    fn reports_dynamic_and_escaped_imports_on_contexts() {
        let source = r#"
            const Context = js.newContext()
            Context:Import(Name)
            Context:Import("./First.ts", Options)
            Context:Import("./\u{46}irst.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert!(discovery.imports.is_empty());
        assert!(discovery.has_dynamic_imports);
    }

    #[test]
    fn rejects_invalid_luau() {
        assert!(discover_js_imports("const =").is_err());
    }
}
