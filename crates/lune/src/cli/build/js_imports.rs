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

#[derive(Default)]
struct ImportVisitor {
    contexts: Vec<BTreeMap<String, Option<String>>>,
    imports: Vec<ImportCall>,
}

struct ImportCall {
    module_root: Option<Option<String>>,
    receiver: String,
    specifier: Option<String>,
}

impl ImportVisitor {
    fn enter_scope(&mut self) {
        self.contexts.push(BTreeMap::new());
    }

    fn lookup(&self, name: &str) -> Option<Option<String>> {
        self.contexts
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn record_binding<'a>(
        &mut self,
        names: impl Iterator<Item = TokenReference>,
        expressions: impl Iterator<Item = &'a Expression>,
    ) {
        for (name, expression) in names.zip(expressions) {
            let name = name.token().to_string();
            let binding = if let Some(module_root) = new_context_root(expression) {
                Some(module_root)
            } else if let Expression::Var(Var::Name(alias)) = expression {
                self.lookup(&alias.token().to_string())
            } else {
                None
            };
            if let Some(binding) = binding {
                if let Some(scope) = self.contexts.last_mut() {
                    scope.insert(name, binding);
                }
            }
        }
    }
}

impl Visitor for ImportVisitor {
    fn visit_block(&mut self, _block: &full_moon::ast::Block) {
        self.enter_scope();
    }

    fn visit_block_end(&mut self, _block: &full_moon::ast::Block) {
        self.contexts.pop();
    }

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
        let module_root = self.lookup(&receiver);
        for suffix in call.suffixes() {
            let Suffix::Call(Call::MethodCall(method)) = suffix else {
                continue;
            };
            if method.name().token().to_string() != "Import" {
                continue;
            }
            self.imports.push(ImportCall {
                receiver: receiver.clone(),
                module_root: module_root.clone(),
                specifier: literal_argument(method.args()),
            });
        }
    }
}

/// Finds literal JavaScript imports on variables created with `newContext`.
///
/// Contexts are tracked lexically, including aliases and literal `ModuleRoot`
/// values, so builds resolve modules exactly like runtime execution.
pub fn discover_js_imports(source: &str) -> Result<JsImportDiscovery, Vec<full_moon::Error>> {
    let ast = full_moon::parse(source)?;
    let mut visitor = ImportVisitor::default();
    visitor.enter_scope();
    visitor.visit_ast(&ast);

    let mut discovery = JsImportDiscovery::default();
    for import in visitor.imports {
        let Some(module_root) = import.module_root else {
            if let Some(specifier) = import.specifier {
                discovery.unknown_specifiers.insert(specifier);
            }
            continue;
        };
        match import.specifier {
            Some(specifier) => {
                discovery.imports.insert((specifier, module_root));
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
    fn pairs_multiple_names_with_their_own_expressions() {
        let source = r#"
            local First, Second =
                js.newContext({ ModuleRoot = "./A" }),
                js.newContext({ ModuleRoot = "./B" })
            First:Import("/Value.ts")
            Second:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![
                ("/Value.ts".to_string(), Some("./A".to_string())),
                ("/Value.ts".to_string(), Some("./B".to_string())),
            ]
        );
    }

    #[test]
    fn respects_lexical_scopes_for_same_names() {
        let source = r#"
            do
                local Context = js.newContext({ ModuleRoot = "./A" })
                Context:Import("/Value.ts")
            end
            do
                local Context = js.newContext({ ModuleRoot = "./B" })
                Context:Import("/Value.ts")
            end
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![
                ("/Value.ts".to_string(), Some("./A".to_string())),
                ("/Value.ts".to_string(), Some("./B".to_string())),
            ]
        );
    }

    #[test]
    fn rejects_invalid_luau() {
        assert!(discover_js_imports("const =").is_err());
    }
}
