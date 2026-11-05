#![allow(clippy::cargo_common_metadata)]

use std::fs;

use lune_std_js::{
    loader::SharedModuleCache,
    module::{DependencyKind, ModuleError, ModuleId},
    resolver::{resolve_module, validate_specifier},
    source::{
        BundleSourceProvider, HostSourceProvider, MemorySourceProvider, ModuleSource,
        SourceProvider,
    },
    store::ModuleGraph,
};
use rquickjs::{AsyncContext, AsyncRuntime, Module, Value};

fn id(path: &str) -> ModuleId {
    ModuleId::parse(path).unwrap()
}

fn memory(modules: &[(&str, &str)]) -> MemorySourceProvider {
    MemorySourceProvider::with_modules(
        modules
            .iter()
            .map(|(path, source)| (id(path), (*source).to_string())),
    )
    .unwrap()
}

#[test]
fn resolves_typescript_extensions_and_index_modules() {
    let provider = memory(&[
        ("/Source/Thing.ts", "export const Value = 42;"),
        ("/Source/Nested/index.mts", "export const Value = 42;"),
    ]);
    let base = id("/Source/Main.ts");

    assert_eq!(
        resolve_module(&provider, &base, "./Thing.js").unwrap(),
        id("/Source/Thing.ts")
    );
    assert_eq!(
        resolve_module(&provider, &base, "./Thing").unwrap(),
        id("/Source/Thing.ts")
    );
    assert_eq!(
        resolve_module(&provider, &base, "./Nested").unwrap(),
        id("/Source/Nested/index.mts")
    );
}

#[test]
fn rejects_bare_specifiers_urls_and_root_escapes() {
    assert!(validate_specifier("package").is_err());
    assert!(validate_specifier("https://example.com/module.js").is_err());

    let provider = memory(&[("/Source/Main.ts", "export {};")]);
    let error = resolve_module(&provider, &id("/Source/Main.ts"), "../Outside.ts").unwrap_err();
    assert!(matches!(error, ModuleError::NotFound(_)));

    let error = resolve_module(&provider, &id("/Main.ts"), "../Outside.ts").unwrap_err();
    assert!(matches!(error, ModuleError::RootEscape(_)));
}

#[test]
fn loads_cycles_and_literal_dynamic_dependencies() {
    let provider = memory(&[
        (
            "/Main.ts",
            r#"
                import { ReadCycle } from "./CycleA.js";
                export async function Read(): Promise<string> {
                    const Extra = await import("./Extra.js");
                    return `${ReadCycle()}${Extra.Value}`;
                }
            "#,
        ),
        (
            "/CycleA.ts",
            r#"import { ReadB } from "./CycleB.js"; export function ReadCycle() { return `A${ReadB()}`; }"#,
        ),
        ("/CycleB.ts", "export function ReadB() { return 'B'; }"),
        ("/Extra.ts", "export const Value = 42;"),
    ]);

    let graph = ModuleGraph::load(&provider, id("/Main.ts")).unwrap();
    assert_eq!(graph.len(), 4);
    let dynamic = graph.get(&id("/Main.ts")).unwrap();
    assert!(
        dynamic
            .dependencies()
            .iter()
            .any(|dependency| dependency.kind == DependencyKind::Dynamic)
    );
}

#[test]
fn evaluates_a_transformed_cycle_with_top_level_await() {
    let provider = memory(&[
        (
            "/Main.ts",
            r#"
                import { ReadCycle } from "./CycleA.js";
                const Extra = await import("./Extra.js");
                export const Result = `${ReadCycle()}${Extra.Value}${await Promise.resolve(42)}`;
            "#,
        ),
        (
            "/CycleA.ts",
            r#"import { ReadB } from "./CycleB.js"; export function ReadCycle() { return `A${ReadB()}`; }"#,
        ),
        ("/CycleB.ts", "export function ReadB() { return 'B'; }"),
        ("/Extra.ts", "export const Value = 40 + 2;"),
    ]);
    let graph = ModuleGraph::load(&provider, id("/Main.ts")).unwrap();
    let cache = SharedModuleCache::new();
    cache.insert_graph(graph);

    async_io::block_on(async {
        let runtime = AsyncRuntime::new().unwrap();
        runtime.set_loader(cache.clone(), cache.clone()).await;
        let context = AsyncContext::full(&runtime).await.unwrap();
        context
            .async_with(async |ctx| {
                let entry = cache.load(&id("/Main.ts")).unwrap();
                let ModuleSource::Generated { code, .. } = entry else {
                    panic!("generated module expected");
                };
                let (module, promise) = Module::declare(ctx.clone(), "/Main.ts", code)
                    .unwrap()
                    .eval()
                    .unwrap();
                promise.into_future::<Value>().await.unwrap();
                let result: String = module.get("Result").unwrap();
                assert_eq!(result, "AB4242");
            })
            .await;
    });
}

#[test]
fn evaluates_from_bundle_without_original_sources() {
    let provider = memory(&[
        (
            "/Main.ts",
            "import { Value } from './Value.js'; export const Result: number = Value + 2;",
        ),
        ("/Value.ts", "export const Value = 40;"),
    ]);
    let graph = ModuleGraph::load(&provider, id("/Main.ts")).unwrap();
    let bundle = BundleSourceProvider::from_modules(graph.into_modules().into_values()).unwrap();
    let bundled_graph = ModuleGraph::load(&bundle, id("/Main.ts")).unwrap();
    assert_eq!(bundled_graph.len(), 2);
    assert!(
        bundled_graph
            .get(&id("/Main.ts"))
            .unwrap()
            .source_map()
            .is_some()
    );
}

#[test]
fn decodes_generated_artifacts_and_their_source_maps() {
    let provider = memory(&[("/Main.ts", "export const Value: number = 40 + 2;")]);
    let graph = ModuleGraph::load(&provider, id("/Main.ts")).unwrap();
    let artifacts = graph.into_modules().into_iter().map(|(id, module)| {
        let (_, code, source_map) = module.into_bundle_parts();
        (
            id.to_string(),
            code.into_bytes(),
            source_map.map(|map| map.to_json_string().into_bytes()),
        )
    });
    let provider = BundleSourceProvider::from_generated(artifacts).unwrap();
    let graph = ModuleGraph::load(&provider, id("/Main.ts")).unwrap();
    let module = graph.get(&id("/Main.ts")).unwrap();
    assert!(module.code().contains("40 + 2"));
    assert!(
        module
            .source_map()
            .unwrap()
            .original_source()
            .unwrap()
            .contains(": number")
    );
}

#[test]
fn host_provider_rejects_symlink_escape() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("Outside.ts");
        fs::write(&outside_file, "export const Value = 42;").unwrap();
        symlink(&outside_file, root.path().join("Link.ts")).unwrap();

        let provider = HostSourceProvider::new(root.path()).unwrap();
        let error = provider.load(&id("/Link.ts")).unwrap_err();
        assert!(matches!(error, ModuleError::SymlinkEscape(_)));
    }
}

#[test]
fn host_provider_reads_only_modules_inside_its_root() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("Main.ts"), "export const Value = 42;").unwrap();
    let provider = HostSourceProvider::new(root.path()).unwrap();

    assert!(provider.exists(&id("/Main.ts")).unwrap());
    assert!(!provider.exists(&id("/Missing.ts")).unwrap());
    assert!(matches!(
        provider.load(&id("/Missing.ts")),
        Err(ModuleError::NotFound(_))
    ));
}
