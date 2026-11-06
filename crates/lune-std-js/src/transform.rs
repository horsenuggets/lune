use std::{collections::BTreeSet, path::Path};

use oxc_allocator::{Allocator, TakeIn};
use oxc_ast::{
    AstBuilder,
    ast::{
        Argument, BindingIdentifier, ExportAllDeclaration, ExportNamedDeclaration, Expression,
        ImportDeclaration, ImportDeclarationSpecifier, ImportExpression, ImportOrExportKind,
        Program, Statement,
    },
};
use oxc_ast_visit::{
    Visit, VisitMut,
    walk::{
        walk_export_all_declaration, walk_export_named_declaration, walk_import_declaration,
        walk_import_expression,
    },
    walk_mut::walk_expression,
};
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_diagnostics::OxcDiagnostic;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

use crate::{
    module::{DependencyKind, ModuleDependency, ModuleError, ModuleId, TransformedModule},
    source_map::PortableSourceMap,
};

/**
    The virtual module that exposes the runtime's computed dynamic import bridge.

    Rewritten modules import it, so the helper resolves through a module binding that a
    user declaration cannot capture.
*/
pub const DYNAMIC_IMPORT_MODULE: &str = "/__lune_internal__/dynamic-import.js";

/** The export name and preferred local binding of the dynamic import bridge. */
pub const DYNAMIC_IMPORT_HELPER: &str = "__luneDynamicImport";

/** Transforms one original JavaScript or TypeScript module into portable ESM. */
pub fn transform_module(id: ModuleId, source: &str) -> Result<TransformedModule, ModuleError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(id.as_str())
        .map_err(|error| ModuleError::UnsupportedExtension(error.to_string()))?;
    let mut parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(ModuleError::Parse(format_diagnostics(parsed.errors)));
    }

    let mut dependencies = DependencyVisitor::default();
    dependencies.visit_program(&parsed.program);
    if let Some(error) = dependencies.error {
        return Err(error);
    }
    let helper = dynamic_import_helper_name(&parsed.program);
    let mut rewriter = DynamicImportRewriter {
        allocator: &allocator,
        helper: &helper,
        module_id: id.as_str(),
        rewritten: false,
    };
    rewriter.visit_program(&mut parsed.program);
    if rewriter.rewritten {
        insert_dynamic_import(&allocator, &mut parsed.program, &helper);
    }

    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .build(&parsed.program);
    if !semantic.errors.is_empty() {
        return Err(ModuleError::Parse(format_diagnostics(semantic.errors)));
    }

    let transformed = Transformer::new(
        &allocator,
        Path::new(id.as_str()),
        &TransformOptions::default(),
    )
    .build_with_scoping(semantic.semantic.into_scoping(), &mut parsed.program);
    if !transformed.errors.is_empty() {
        return Err(ModuleError::Transform(format_diagnostics(
            transformed.errors,
        )));
    }

    // Collect runtime dependencies after erasure so `import type` declarations do not
    // become mandatory modules. Dynamic imports were rewritten above, so reuse the
    // original pass for those.
    let mut runtime_dependencies = DependencyVisitor::default();
    runtime_dependencies.visit_program(&parsed.program);
    if let Some(error) = runtime_dependencies.error {
        return Err(error);
    }
    let mut merged = runtime_dependencies
        .specifiers
        .into_iter()
        .filter(|dependency| dependency.kind == DependencyKind::Static)
        .collect::<BTreeSet<_>>();
    merged.extend(
        dependencies
            .specifiers
            .into_iter()
            .filter(|dependency| dependency.kind == DependencyKind::Dynamic),
    );

    let generated = Codegen::new()
        .with_options(CodegenOptions {
            source_map_path: Some(Path::new(id.as_str()).to_path_buf()),
            ..CodegenOptions::default()
        })
        .with_scoping(Some(transformed.scoping))
        .build(&parsed.program);
    let source_map = generated
        .map
        .map(|map| PortableSourceMap::new(map, &id, source));

    Ok(TransformedModule {
        code: generated.code,
        dependencies: merged.into_iter().collect(),
        has_computed_dynamic_import: dependencies.has_computed_dynamic_import,
        id,
        source_map,
    })
}

/** Reanalyzes generated ESM from a bundle without discarding its original source map. */
pub fn inspect_generated_module(
    id: ModuleId,
    code: &str,
    source_map: Option<PortableSourceMap>,
) -> Result<TransformedModule, ModuleError> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(id.as_str())
        .map_err(|error| ModuleError::UnsupportedExtension(error.to_string()))?;
    let parsed = Parser::new(&allocator, code, source_type).parse();
    if !parsed.errors.is_empty() {
        return Err(ModuleError::Parse(format_diagnostics(parsed.errors)));
    }

    let mut dependencies = DependencyVisitor::default();
    dependencies.visit_program(&parsed.program);
    if let Some(error) = dependencies.error {
        return Err(error);
    }

    Ok(TransformedModule {
        code: code.to_string(),
        dependencies: dependencies.specifiers.into_iter().collect(),
        has_computed_dynamic_import: dependencies.has_computed_dynamic_import,
        id,
        source_map,
    })
}

/// Picks a helper binding name that no declaration in the module can shadow.
fn dynamic_import_helper_name(program: &Program) -> String {
    let mut collector = BindingNameCollector::default();
    collector.visit_program(program);
    let mut name = DYNAMIC_IMPORT_HELPER.to_string();
    while collector.names.contains(&name) {
        name.push('$');
    }
    name
}

#[derive(Default)]
struct BindingNameCollector {
    names: BTreeSet<String>,
}

impl<'a> Visit<'a> for BindingNameCollector {
    fn visit_binding_identifier(&mut self, identifier: &BindingIdentifier<'a>) {
        self.names.insert(identifier.name.to_string());
    }
}

/// Imports the dynamic import bridge under the module's private binding.
fn insert_dynamic_import<'a>(allocator: &'a Allocator, program: &mut Program<'a>, helper: &str) {
    let builder = AstBuilder::new(allocator);
    let span = program.span;
    let specifier = builder.import_specifier(
        span,
        builder.module_export_name_identifier_name(span, DYNAMIC_IMPORT_HELPER),
        builder.binding_identifier(span, builder.ident(helper)),
        ImportOrExportKind::Value,
    );
    let declaration = builder.import_declaration(
        span,
        Some(
            builder.vec_from_array([ImportDeclarationSpecifier::ImportSpecifier(
                builder.alloc_import_specifier(
                    span,
                    specifier.imported,
                    specifier.local,
                    specifier.import_kind,
                ),
            )]),
        ),
        builder.string_literal(span, DYNAMIC_IMPORT_MODULE, None),
        None,
        None::<oxc_allocator::Box<_>>,
        ImportOrExportKind::Value,
    );
    program
        .body
        .insert(0, Statement::ImportDeclaration(builder.alloc(declaration)));
}

struct DynamicImportRewriter<'a> {
    allocator: &'a Allocator,
    helper: &'a str,
    module_id: &'a str,
    rewritten: bool,
}

impl<'a> VisitMut<'a> for DynamicImportRewriter<'a> {
    fn visit_expression(&mut self, expression: &mut Expression<'a>) {
        *expression = match expression.take_in(self.allocator) {
            Expression::ImportExpression(mut import)
                if !matches!(&import.source, Expression::StringLiteral(_)) =>
            {
                let import = import.take_in(self.allocator);
                let builder = AstBuilder::new(self.allocator);
                let span = import.span;
                self.rewritten = true;
                let callee = builder.expression_identifier(span, builder.ident(self.helper));
                let module_id = builder.expression_string_literal(span, self.module_id, None);
                // The bridge coerces the specifier itself, so a failure rejects the
                // returned promise the way `import()` does.
                let arguments = builder
                    .vec_from_array([Argument::from(module_id), Argument::from(import.source)]);
                Expression::CallExpression(builder.alloc_call_expression(
                    span,
                    callee,
                    None::<oxc_allocator::Box<_>>,
                    arguments,
                    false,
                ))
            }
            other => other,
        };
        walk_expression(self, expression);
    }
}

#[derive(Default)]
struct DependencyVisitor {
    error: Option<ModuleError>,
    has_computed_dynamic_import: bool,
    specifiers: BTreeSet<ModuleDependency>,
}

impl DependencyVisitor {
    fn insert(&mut self, kind: DependencyKind, specifier: &str) {
        self.specifiers.insert(ModuleDependency {
            kind,
            specifier: specifier.to_string(),
        });
    }

    fn reject_attributes(&mut self, specifier: &str) {
        self.error = Some(ModuleError::ImportAttributes(format!(
            "\"{specifier}\" uses import attributes"
        )));
    }
}

impl<'a> Visit<'a> for DependencyVisitor {
    fn visit_import_declaration(&mut self, declaration: &ImportDeclaration<'a>) {
        if declaration.with_clause.is_some() {
            self.reject_attributes(&declaration.source.value);
        }
        if declaration.phase.is_some() {
            self.error = Some(ModuleError::Transform(
                "Import phases are not supported".to_string(),
            ));
        }
        // The dynamic import bridge is supplied by the runtime, not the source graph.
        if declaration.source.value != DYNAMIC_IMPORT_MODULE {
            self.insert(DependencyKind::Static, &declaration.source.value);
        }
        walk_import_declaration(self, declaration);
    }

    fn visit_export_named_declaration(&mut self, declaration: &ExportNamedDeclaration<'a>) {
        if declaration.with_clause.is_some() {
            self.reject_attributes(
                declaration
                    .source
                    .as_ref()
                    .map_or("export declaration", |source| &source.value),
            );
        }
        if let Some(source) = &declaration.source {
            self.insert(DependencyKind::Static, &source.value);
        }
        walk_export_named_declaration(self, declaration);
    }

    fn visit_export_all_declaration(&mut self, declaration: &ExportAllDeclaration<'a>) {
        if declaration.with_clause.is_some() {
            self.reject_attributes(&declaration.source.value);
        }
        self.insert(DependencyKind::Static, &declaration.source.value);
        walk_export_all_declaration(self, declaration);
    }

    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        if expression.options.is_some() {
            self.reject_attributes("dynamic import");
        }
        if expression.phase.is_some() {
            self.error = Some(ModuleError::Transform(
                "Import phases are not supported".to_string(),
            ));
        }
        match &expression.source {
            Expression::StringLiteral(literal) => {
                self.insert(DependencyKind::Dynamic, &literal.value);
            }
            _ => self.has_computed_dynamic_import = true,
        }
        walk_import_expression(self, expression);
    }
}

fn format_diagnostics(diagnostics: impl IntoIterator<Item = OxcDiagnostic>) -> String {
    diagnostics
        .into_iter()
        .map(|diagnostic| diagnostic.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_types_and_collects_module_dependencies() {
        let module = transform_module(
            ModuleId::parse("/Source/Catalog.ts").unwrap(),
            r#"
                import type { Story } from "./Story.js";
                import { Render } from "./Render.js";
                export { Extra } from "./Extra.js";
                export * from "./More.js";
                export function Create(story: Story): string {
                    return Render(story.Name);
                }
            "#,
        )
        .unwrap();

        assert!(!module.code().contains(": Story"));
        assert!(!module.code().contains("import type"));
        let dependencies = module.dependencies();
        assert_eq!(dependencies.len(), 3);
        assert!(
            dependencies
                .iter()
                .all(|dependency| dependency.specifier != "./Story.js")
        );
        assert!(dependencies.iter().any(|dependency| {
            dependency.specifier == "./More.js" && dependency.kind == DependencyKind::Static
        }));
    }

    #[test]
    fn distinguishes_literal_and_computed_dynamic_imports() {
        let module = transform_module(
            ModuleId::parse("/Source/Dynamic.ts").unwrap(),
            r#"
                export async function Load(Name: string) {
                    await import("./Static.js");
                    return import(`./${Name}.js`);
                }
            "#,
        )
        .unwrap();

        assert!(module.has_computed_dynamic_import());
        // The bridge arrives through a runtime-owned module binding.
        assert!(module.code().contains(
            "import { __luneDynamicImport } from \"/__lune_internal__/dynamic-import.js\""
        ));
        assert!(
            module
                .code()
                .contains("__luneDynamicImport(\"/Source/Dynamic.ts\"")
        );
        // The bridge itself owns the ToString coercion.
        assert_eq!(module.dependencies().len(), 1);
        assert_eq!(module.dependencies()[0].kind, DependencyKind::Dynamic);
        assert_eq!(module.dependencies()[0].specifier, "./Static.js");
    }

    #[test]
    fn computed_dynamic_import_ignores_user_bindings() {
        let module = transform_module(
            ModuleId::parse("/Source/Shadow.ts").unwrap(),
            r#"
                const __luneDynamicImport = () => {
                    throw new Error("shadowed");
                };
                export const Helper = __luneDynamicImport;
                export function Load(Name: string) {
                    return import(`./${Name}.js`);
                }
            "#,
        )
        .unwrap();

        // A user declaration forces a different, unshadowable binding name.
        assert!(module.code().contains("__luneDynamicImport$"));
        assert!(module.code().contains("Helper = __luneDynamicImport;"));
    }

    #[test]
    fn rejects_import_attributes() {
        let error = transform_module(
            ModuleId::parse("/Source/Data.ts").unwrap(),
            r#"import Data from "./Data.json" with { type: "json" };"#,
        )
        .unwrap_err();
        assert!(matches!(error, ModuleError::ImportAttributes(_)));
    }

    #[test]
    fn creates_a_portable_source_map() {
        let module = transform_module(
            ModuleId::parse("/Source/Error.ts").unwrap(),
            "export function Fail(): never {\n    throw new Error(\"Expected\");\n}\n",
        )
        .unwrap();
        let map = module.source_map().unwrap();
        let json = map.to_json_string();
        assert!(json.contains("/Source/Error.ts"));
        assert!(json.contains("sourcesContent"));
        assert!(map.original_source().unwrap().contains("Expected"));
    }

    #[test]
    fn rejects_invalid_typescript() {
        let error = transform_module(
            ModuleId::parse("/Source/Invalid.ts").unwrap(),
            "const Value: = 42;",
        )
        .unwrap_err();
        assert!(matches!(error, ModuleError::Parse(_)));
    }
}
