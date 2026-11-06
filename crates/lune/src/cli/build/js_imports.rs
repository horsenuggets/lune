use std::collections::{BTreeMap, BTreeSet};

use full_moon::{
    ast::{
        Assignment, Call, Expression, Field, FunctionArgs, FunctionCall, Index, LocalAssignment,
        Prefix, Suffix, TableConstructor, Var, luau::ConstAssignment,
    },
    tokenizer::{TokenReference, TokenType},
    visitors::Visitor,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct JsImportDiscovery {
    /// Every discovered context root, even when no literal import was found.
    pub context_roots: BTreeSet<Option<String>>,
    /// Literal imports paired with the receiving context's `ModuleRoot`, if any.
    pub imports: BTreeSet<(String, Option<String>)>,
    pub has_dynamic_imports: bool,
    /// Receivers whose context options are not literals, so their root is unknown.
    pub unresolved_receivers: BTreeSet<String>,
    /// Literal imports on receivers that could not be traced to a context.
    pub unknown_specifiers: BTreeSet<String>,
}

/// What a lexical name is currently bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Binding {
    /// A JavaScript context, with its literal `ModuleRoot` if it had one.
    Context(Option<String>),
    /// A context created from options the build cannot inspect.
    UnresolvedContext,
    /// Any other value, including an alias to an unresolved context.
    Other,
}

struct ImportCall {
    binding: Option<Binding>,
    receiver: String,
    specifier: Option<String>,
}

#[derive(Default)]
struct ImportVisitor {
    scopes: Vec<BTreeMap<String, Binding>>,
    imports: Vec<ImportCall>,
    /// Function parameters are visited before their body block, so they are
    /// recorded once that block opens its scope.
    pending_parameters: Vec<String>,
    /// Assignments inside a function body are not known to run, so they stay
    /// scoped to that body instead of mutating the enclosing environment.
    function_depth: usize,
    roots: BTreeSet<Option<String>>,
}

impl ImportVisitor {
    fn enter_scope(&mut self) {
        let mut scope = BTreeMap::new();
        for name in self.pending_parameters.drain(..) {
            scope.insert(name, Binding::Other);
        }
        self.scopes.push(scope);
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    fn binding_for(&self, expression: &Expression) -> Binding {
        if let Some(root) = new_context_root(expression) {
            return match root {
                ContextRoot::Known(module_root) => Binding::Context(module_root),
                ContextRoot::Unresolved => Binding::UnresolvedContext,
            };
        }
        match expression {
            Expression::Var(Var::Name(alias)) => self
                .lookup(&alias.token().to_string())
                .unwrap_or(Binding::Other),
            _ => Binding::Other,
        }
    }

    /// Resolves declared names against the current environment.
    ///
    /// Declarations without an initializer still shadow outer names, and every
    /// right-hand side is resolved before any target is updated.
    fn declared_bindings<'a>(
        &self,
        targets: impl Iterator<Item = (TokenReference, Option<&'a Expression>)>,
    ) -> Vec<(String, Binding)> {
        targets
            .map(|(name, expression)| {
                let binding =
                    expression.map_or(Binding::Other, |expression| self.binding_for(expression));
                (name.token().to_string(), binding)
            })
            .collect()
    }

    /// Installs freshly declared names in the innermost scope.
    fn declare(&mut self, bindings: Vec<(String, Binding)>) {
        for (name, binding) in bindings {
            self.track_root(&binding);
            if let Some(scope) = self.scopes.last_mut() {
                scope.insert(name, binding);
            }
        }
    }

    /// Applies a reassignment after its right-hand side has been visited.
    fn assign(&mut self, bindings: Vec<(String, Binding)>) {
        for (name, binding) in bindings {
            self.track_root(&binding);
            if self.function_depth > 0 {
                // The function may never run, so keep the write inside it.
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(name, binding);
                }
                continue;
            }
            // Reassignment updates the nearest declaration, or creates a global
            // binding when the name was not declared in this file.
            let existing = self
                .scopes
                .iter_mut()
                .enumerate()
                .rev()
                .find_map(|(index, scope)| scope.contains_key(&name).then_some(index));
            let index = existing.unwrap_or(0);
            if let Some(scope) = self.scopes.get_mut(index) {
                scope.insert(name, binding);
            }
        }
    }

    fn track_root(&mut self, binding: &Binding) {
        if let Binding::Context(module_root) = binding {
            self.roots.insert(module_root.clone());
        }
    }
}

impl Visitor for ImportVisitor {
    fn visit_function_body(&mut self, body: &full_moon::ast::FunctionBody) {
        self.pending_parameters = body
            .parameters()
            .iter()
            .filter_map(|parameter| match parameter {
                full_moon::ast::Parameter::Name(name) => Some(name.token().to_string()),
                _ => None,
            })
            .collect();
        self.function_depth += 1;
    }

    fn visit_function_body_end(&mut self, _body: &full_moon::ast::FunctionBody) {
        self.function_depth -= 1;
    }

    fn visit_block(&mut self, _block: &full_moon::ast::Block) {
        self.enter_scope();
    }

    fn visit_block_end(&mut self, _block: &full_moon::ast::Block) {
        self.scopes.pop();
    }

    fn visit_const_assignment_end(&mut self, assignment: &ConstAssignment) {
        let bindings = self.declared_bindings(pair_targets(
            assignment.names().iter().cloned().enumerate(),
            assignment.expressions().iter(),
        ));
        self.declare(bindings);
    }

    fn visit_local_assignment_end(&mut self, assignment: &LocalAssignment) {
        let bindings = self.declared_bindings(pair_targets(
            assignment.names().iter().cloned().enumerate(),
            assignment.expressions().iter(),
        ));
        self.declare(bindings);
    }

    fn visit_assignment_end(&mut self, assignment: &Assignment) {
        let bindings = self.declared_bindings(pair_targets(
            assignment.variables().iter().enumerate().filter_map(
                |(index, variable)| match variable {
                    Var::Name(target) => Some((index, target.clone())),
                    _ => None,
                },
            ),
            assignment.expressions().iter(),
        ));
        self.assign(bindings);
    }

    fn visit_function_call(&mut self, call: &FunctionCall) {
        let Prefix::Name(receiver) = call.prefix() else {
            return;
        };
        let receiver = receiver.token().to_string();
        let binding = self.lookup(&receiver);
        for suffix in call.suffixes() {
            let Suffix::Call(Call::MethodCall(method)) = suffix else {
                continue;
            };
            if method.name().token().to_string() != "Import" {
                continue;
            }
            self.imports.push(ImportCall {
                binding: binding.clone(),
                receiver: receiver.clone(),
                specifier: literal_argument(method.args()),
            });
        }
    }
}

/// Pairs declared names with their own right-hand side, if any.
fn pair_targets<'a>(
    names: impl Iterator<Item = (usize, TokenReference)>,
    expressions: impl Iterator<Item = &'a Expression>,
) -> impl Iterator<Item = (TokenReference, Option<&'a Expression>)> {
    let expressions = expressions.collect::<Vec<_>>();
    names.map(move |(index, name)| (name, expressions.get(index).copied()))
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
    discovery.context_roots.extend(visitor.roots);
    for import in visitor.imports {
        match import.binding {
            Some(Binding::Context(module_root)) => match import.specifier {
                Some(specifier) => {
                    discovery.imports.insert((specifier, module_root));
                }
                None => discovery.has_dynamic_imports = true,
            },
            Some(Binding::UnresolvedContext) => {
                discovery.unresolved_receivers.insert(import.receiver);
                if import.specifier.is_none() {
                    discovery.has_dynamic_imports = true;
                }
            }
            _ => {
                if let Some(specifier) = import.specifier {
                    discovery.unknown_specifiers.insert(specifier);
                }
            }
        }
    }
    Ok(discovery)
}

enum ContextRoot {
    /// The options are fully literal, with `ModuleRoot` when it was set.
    Known(Option<String>),
    /// The options are not literals, so the actual root is unknowable here.
    Unresolved,
}

/// Returns the context root when the expression creates a context.
fn new_context_root(expression: &Expression) -> Option<ContextRoot> {
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

/// Returns whether the expression is the literal `nil`.
fn is_nil(expression: &Expression) -> bool {
    matches!(expression, Expression::Symbol(token) if token.token().to_string() == "nil")
}

fn module_root_from_args(arguments: &FunctionArgs) -> ContextRoot {
    match arguments {
        FunctionArgs::Parentheses { arguments, .. } => {
            let mut arguments = arguments.iter();
            match (arguments.next(), arguments.next()) {
                (None, _) => ContextRoot::Known(None),
                (Some(expression), None) if is_nil(expression) => ContextRoot::Known(None),
                (Some(Expression::TableConstructor(table)), None) => module_root_from_table(table),
                (Some(Expression::Parentheses { expression, .. }), None) => {
                    match expression.as_ref() {
                        expression if is_nil(expression) => ContextRoot::Known(None),
                        Expression::TableConstructor(table) => module_root_from_table(table),
                        _ => ContextRoot::Unresolved,
                    }
                }
                _ => ContextRoot::Unresolved,
            }
        }
        // `js.newContext { ModuleRoot = "./Source" }`
        FunctionArgs::TableConstructor(table) => module_root_from_table(table),
        _ => ContextRoot::Known(None),
    }
}

fn module_root_from_table(table: &TableConstructor) -> ContextRoot {
    let mut module_root = None;
    for field in table.fields() {
        let Field::NameKey { key, value, .. } = field else {
            continue;
        };
        if key.token().to_string() != "ModuleRoot" {
            continue;
        }
        // An explicit `nil` uses the default root, just like omitting the field.
        let Expression::String(token) = value.as_ref() else {
            return match value.as_ref() {
                expression if is_nil(expression) => ContextRoot::Known(None),
                _ => ContextRoot::Unresolved,
            };
        };
        let Some(root) = string_literal(token) else {
            return ContextRoot::Unresolved;
        };
        module_root = Some(root);
    }
    ContextRoot::Known(module_root)
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
    fn inner_declarations_mask_outer_contexts() {
        let source = r#"
            const Context = js.newContext({ ModuleRoot = "./A" })
            do
                local Context = { Import = function() end }
                Context:Import("asset.json")
            end
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./A".to_string()))]
        );
        assert_eq!(
            discovery.unknown_specifiers.into_iter().collect::<Vec<_>>(),
            vec!["asset.json".to_string()]
        );
    }

    #[test]
    fn parameters_mask_outer_contexts() {
        let source = r#"
            local Context = js.newContext({ ModuleRoot = "./A" })
            local function Custom(Context: any)
                Context:Import("asset.json")
            end
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./A".to_string()))]
        );
        assert_eq!(
            discovery.unknown_specifiers.into_iter().collect::<Vec<_>>(),
            vec!["asset.json".to_string()]
        );
    }

    #[test]
    fn declarations_without_initializers_shadow_outer_contexts() {
        let source = r#"
            local Context = js.newContext({ ModuleRoot = "./A" })
            do
                local Context
                Context = js.newContext({ ModuleRoot = "./B" })
                Context:Import("/Value.ts")
            end
            Context:Import("/Value.ts")
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
    fn imports_on_the_right_hand_side_see_the_previous_binding() {
        let source = r#"
            local Context = js.newContext({ ModuleRoot = "./A" })
            Context = Context:Import("./Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("./Value.ts".to_string(), Some("./A".to_string()))]
        );
    }

    #[test]
    fn swaps_resolve_against_the_previous_environment() {
        let source = r#"
            local First = js.newContext({ ModuleRoot = "./A" })
            local Second = js.newContext({ ModuleRoot = "./B" })
            First, Second = Second, First
            Second:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./A".to_string()))]
        );
    }

    #[test]
    fn uncalled_function_bodies_do_not_mutate_outer_bindings() {
        let source = r#"
            local Context = js.newContext({ ModuleRoot = "./A" })
            local function CloseLater()
                Context = nil
            end
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./A".to_string()))]
        );
    }

    #[test]
    fn treats_literal_nil_options_as_the_default_root() {
        let source = r#"
            local First = js.newContext(nil)
            local Second = js.newContext({ ModuleRoot = nil })
            First:Import("/Value.ts")
            Second:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), None)]
        );
        assert!(discovery.unresolved_receivers.is_empty());
    }

    #[test]
    fn reassignment_updates_the_tracked_root() {
        let source = r#"
            local Context = js.newContext({ ModuleRoot = "./A" })
            Context = js.newContext({ ModuleRoot = "./B" })
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./B".to_string()))]
        );
    }

    #[test]
    fn supports_table_constructor_arguments() {
        let source = r#"
            const Context = js.newContext { ModuleRoot = "./Source" }
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery.imports.into_iter().collect::<Vec<_>>(),
            vec![("/Value.ts".to_string(), Some("./Source".to_string()))]
        );
    }

    #[test]
    fn reports_contexts_built_from_opaque_options() {
        let source = r#"
            const Options = { ModuleRoot = "./Source" }
            const Context = js.newContext(Options)
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert!(discovery.imports.is_empty());
        assert_eq!(
            discovery
                .unresolved_receivers
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["Context".to_string()]
        );
    }

    #[test]
    fn reports_nonliteral_module_roots() {
        let source = r#"
            const Source = "./Source"
            const Context = js.newContext({ ModuleRoot = Source })
            Context:Import("/Value.ts")
        "#;
        let discovery = discover_js_imports(source).unwrap();
        assert_eq!(
            discovery
                .unresolved_receivers
                .into_iter()
                .collect::<Vec<_>>(),
            vec!["Context".to_string()]
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
