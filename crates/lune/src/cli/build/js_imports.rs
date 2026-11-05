use std::collections::BTreeSet;

use full_moon::{
    ast::{Expression, FunctionArgs, MethodCall},
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
    discovery: JsImportDiscovery,
}

impl Visitor for ImportVisitor {
    fn visit_method_call(&mut self, call: &MethodCall) {
        if call.name().token().to_string() != "Import" {
            return;
        }

        if let Some(specifier) = literal_argument(call.args()) {
            self.discovery.specifiers.insert(specifier);
        } else {
            self.discovery.has_dynamic_imports = true;
        }
    }
}

/// Finds literal JavaScript imports without matching comments, strings, or dynamic expressions.
pub fn discover_js_imports(source: &str) -> Result<JsImportDiscovery, Vec<full_moon::Error>> {
    let ast = full_moon::parse(source)?;
    let mut visitor = ImportVisitor::default();
    visitor.visit_ast(&ast);
    Ok(visitor.discovery)
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
    fn finds_literal_method_imports() {
        let source = r#"
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
    fn ignores_comments_strings_and_other_calls() {
        let source = r#"
            -- Context:Import("./Comment.ts")
            const Text = 'Context:Import("./String.ts")'
            const Value = Context.import("./Property.ts")
            const Other = Context:Evaluate("./Evaluate.ts")
        "#;
        assert!(specifiers(source).is_empty());
    }

    #[test]
    fn reports_dynamic_and_escaped_imports() {
        let source = r#"
            const First = Context:Import(Name)
            const Second = Context:Import("./First.ts", Options)
            const Third = Context:Import("./\u{46}irst.ts")
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
