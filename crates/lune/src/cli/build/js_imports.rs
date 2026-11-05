use std::collections::BTreeSet;

use full_moon::{
    ast::{
        Call, Expression, FunctionArgs, FunctionCall, Index, LocalAssignment, Prefix, Suffix,
        luau::ConstAssignment,
    },
    tokenizer::{TokenReference, TokenType},
    visitors::Visitor,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct JsImportDiscovery {
    pub specifiers: BTreeSet<String>,
    pub has_dynamic_imports: bool,
}

#[derive(Default)]
struct ImportVisitor {
    context_names: BTreeSet<String>,
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
        if expressions.into_iter().any(is_new_context_call) {
            self.context_names
                .extend(names.map(|name| name.token().to_string()));
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
            let specifier = literal_argument(method.args());
            self.imports.push(ImportCall {
                receiver: receiver.clone(),
                specifier,
            });
        }
    }
}

/// Finds literal JavaScript imports on variables created with `newContext`.
///
/// Restricting to known context receivers keeps unrelated `:Import(...)` calls from
/// pulling nonexistent JavaScript modules into standalone builds.
pub fn discover_js_imports(source: &str) -> Result<JsImportDiscovery, Vec<full_moon::Error>> {
    let ast = full_moon::parse(source)?;
    let mut visitor = ImportVisitor::default();
    visitor.visit_ast(&ast);

    let mut discovery = JsImportDiscovery::default();
    for import in visitor.imports {
        if !visitor.context_names.contains(&import.receiver) {
            continue;
        }
        match import.specifier {
            Some(specifier) => {
                discovery.specifiers.insert(specifier);
            }
            None => discovery.has_dynamic_imports = true,
        }
    }
    Ok(discovery)
}

fn is_new_context_call(expression: &Expression) -> bool {
    let Expression::FunctionCall(call) = expression else {
        return false;
    };
    let suffixes = call.suffixes().collect::<Vec<_>>();
    suffixes.windows(2).any(|pair| {
        matches!(
            pair,
            [
                Suffix::Index(Index::Dot { name, .. }),
                Suffix::Call(Call::AnonymousCall(_))
            ] if name.token().to_string() == "newContext"
        )
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
            .specifiers
            .into_iter()
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
    fn ignores_unrelated_import_methods_and_other_calls() {
        let source = r#"
            const Importer = { Import = function() end }
            Importer:Import("asset.json")
            local Other = Importer:Import("another.json")
            local Value = Context:Evaluate("./Evaluate.ts")
            local Text = 'Context:Import("./String.ts")'
            -- Context:Import("./Comment.ts")
        "#;
        assert!(specifiers(source).is_empty());
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
        assert!(discovery.specifiers.is_empty());
        assert!(discovery.has_dynamic_imports);
    }

    #[test]
    fn rejects_invalid_luau() {
        assert!(discover_js_imports("const =").is_err());
    }
}
