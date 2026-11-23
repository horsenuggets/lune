use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lune_std_actor::{ActorLauncher, ActorLink, ActorOutcome, spawn_actor_thread};
use lune_utils::{
    path::clean_path,
    process::{ProcessArgs, ProcessEnv, ProcessJitEnablement},
};
use mlua::prelude::*;

#[cfg(feature = "std-js")]
use super::{BundledJsModules, BundledJsRoots};
use super::{
    Runtime,
    runtime::{BundledAliases, BundledFiles, ExecutablePath, strip_shebang},
};

/**
    The configuration an actor's runtime inherits from the runtime that started it.
*/
#[derive(Clone)]
pub(crate) struct WorkerConfig {
    pub(crate) args: ProcessArgs,
    pub(crate) env: ProcessEnv,
    pub(crate) jit: ProcessJitEnablement,
    pub(crate) bundled_files: Arc<BundledFiles>,
    pub(crate) bundled_aliases: BundledAliases,
    #[cfg(feature = "std-js")]
    pub(crate) bundled_js_modules: Option<BundledJsModules>,
    #[cfg(feature = "std-js")]
    pub(crate) bundled_js_roots: BundledJsRoots,
    pub(crate) executable_path: ExecutablePath,
}

/**
    Resolves an actor script path. Paths starting with `./` or `../` are relative to the
    calling script, like `require`, and other paths are used as given.
*/
fn resolve_path(lua: &Lua, path: &str) -> LuaResult<PathBuf> {
    if !(path.starts_with("./") || path.starts_with("../")) {
        return Ok(clean_path(path));
    }
    let caller = lune_std::get_calling_script_path(lua)?.ok_or_else(|| {
        LuaError::runtime(format!(
            "Cannot resolve the actor path \"{path}\" without a calling script."
        ))
    })?;
    let file = lune_std::script_file(lua, Path::new(&caller));
    let directory = file.parent().unwrap_or_else(|| Path::new(""));
    Ok(clean_path(directory.join(path)))
}

fn run_actor(config: WorkerConfig, path: PathBuf, link: Arc<ActorLink>) -> ActorOutcome {
    async_io::block_on(async {
        let bundled = !config.bundled_files.is_empty() || config.executable_path.is_some();
        let files = Arc::clone(&config.bundled_files);
        let mut rt = match Runtime::new_actor(config, Arc::clone(&link)) {
            Ok(rt) => rt,
            Err(error) => {
                return ActorOutcome::from_failure(
                    &link,
                    format!("Failed to create the actor runtime. {error}"),
                );
            }
        };
        let result = if bundled {
            let Some(file) = lune_std::resolve_bundled_file(&files, &path) else {
                return ActorOutcome::from_failure(
                    &link,
                    format!(
                        "Actor script \"{}\" is not embedded. Include it with lune build --include.",
                        path.display()
                    ),
                );
            };
            let source = files
                .get(&file.to_string_lossy().to_string())
                .expect("a resolved bundled file exists");
            rt.run_source(file.to_string_lossy(), strip_shebang(source.clone()))
                .await
        } else {
            rt.run_file(path).await
        };
        match result {
            Ok(values) => ActorOutcome::from_run(
                &link,
                values.code,
                &values.errors,
                values.finished,
                &values.values,
            ),
            Err(error) => ActorOutcome::from_failure(&link, error.to_string()),
        }
    })
}

/**
    Creates the launcher that starts each actor as a new runtime on its own OS thread.
*/
pub(crate) fn launcher(config: WorkerConfig) -> ActorLauncher {
    ActorLauncher::new(move |lua, path, link| {
        let path = resolve_path(lua, path)?;
        let config = config.clone();
        spawn_actor_thread(link, move |link| run_actor(config, path, link)).map_err(|error| {
            LuaError::runtime(format!("Failed to start a thread for the actor. {error}"))
        })
    })
}

#[cfg(test)]
mod tests {
    use std::thread;

    use lune_std_actor::TERMINATED_MESSAGE;

    use super::*;

    const MISSING_SCRIPT: &str = "tests/actor/workers/this-script-does-not-exist.luau";

    fn config() -> WorkerConfig {
        Runtime::new().unwrap().worker_config()
    }

    fn failure(outcome: ActorOutcome) -> String {
        match outcome {
            ActorOutcome::Failed(message) => message,
            ActorOutcome::Completed(_) => panic!("the actor should have failed"),
        }
    }

    fn check_relative_worker_path(remapped: bool) {
        let directory = tempfile::tempdir().unwrap();
        let package = directory.path().join("Package");
        let source = if remapped {
            package.join("Source")
        } else {
            package.clone()
        };
        std::fs::create_dir_all(&source).unwrap();
        let entry = if remapped {
            source.join("Entry.luau")
        } else {
            source.join("init.luau")
        };
        std::fs::write(
            &entry,
            r#"
                const actor = require("@lune/actor")
                assert(require("./Dependency") == "actual", "Requires must use the actual source file.")
                const worker = actor.spawn("./Worker.luau")
                const ok, result = worker:join()
                assert(ok, result)
                assert(result == "actual", "The worker must be beside the actual source file.")
            "#,
        )
        .unwrap();
        std::fs::write(source.join("Worker.luau"), "return 'actual'").unwrap();
        std::fs::write(source.join("Dependency.luau"), "return 'actual'").unwrap();
        std::fs::write(directory.path().join("Worker.luau"), "return 'wrong'").unwrap();
        std::fs::write(directory.path().join("Dependency.luau"), "return 'wrong'").unwrap();
        if remapped {
            std::fs::write(
                package.join("default.project.json"),
                r#"{"tree":{"$path":"Source/Entry.luau"}}"#,
            )
            .unwrap();
            std::fs::write(package.join("Worker.luau"), "return 'wrong'").unwrap();
            std::fs::write(package.join("Dependency.luau"), "return 'wrong'").unwrap();
        }
        let result = async_io::block_on(async {
            Runtime::new()
                .unwrap()
                .run_file(if remapped { package } else { entry })
                .await
                .unwrap()
        });
        assert_eq!(result.status(), 0, "{:?}", result.errors);
    }

    #[test]
    fn relative_workers_resolve_beside_init_entrypoints() {
        check_relative_worker_path(false);
    }

    #[test]
    fn relative_workers_resolve_beside_project_remapped_entrypoints() {
        check_relative_worker_path(true);
    }

    #[test]
    fn bundled_workers_resolve_extensions_and_directory_modules_without_host_files() {
        for filename in [
            "/Worker.luau",
            "/Worker.lua",
            "/Worker/init.luau",
            "/Worker/init.lua",
        ] {
            let files = BundledFiles::from([(
                filename.to_string(),
                b"#!/usr/bin/env lune\nreturn 'embedded'".to_vec(),
            )]);
            let result = async_io::block_on(async {
                Runtime::new()
                    .unwrap()
                    .with_bundled_files(files)
                    .run_source(
                        "/Main.luau",
                        r#"
                        const actor = require("@lune/actor")
                        for _, path in {
                            "./Worker", "Worker", "/Worker", "Unused/../Worker",
                            "./Unused/../Worker", "Folder/.././Worker", "/Folder/../Worker", "Worker/",
                        } do
                            const ok, result = actor.spawn(path):join()
                            assert(ok, result)
                            assert(result == "embedded")
                        end
                    "#,
                    )
                    .await
                    .unwrap()
            });
            assert_eq!(result.status(), 0, "{filename} gave {:?}", result.errors);
        }
    }

    #[test]
    fn bundled_workers_never_fall_back_to_existing_host_files() {
        let directory = tempfile::tempdir().unwrap();
        let worker = directory.path().join("Worker.luau");
        std::fs::write(&worker, "return 'host'").unwrap();
        let config = Runtime::new()
            .unwrap()
            .with_bundled_files(BundledFiles::from([(
                "/Other.luau".to_string(),
                b"return true".to_vec(),
            )]))
            .worker_config();
        let outcome =
            thread::spawn(move || run_actor(config, worker, ActorLink::for_testing(false)))
                .join()
                .unwrap();
        assert!(failure(outcome).contains("not embedded"));
    }

    #[test]
    fn run_failures_of_a_live_actor_keep_their_own_message() {
        let outcome = thread::spawn(|| {
            run_actor(
                config(),
                PathBuf::from(MISSING_SCRIPT),
                ActorLink::for_testing(false),
            )
        })
        .join()
        .unwrap();
        assert_ne!(failure(outcome), TERMINATED_MESSAGE);
    }

    #[test]
    fn run_failures_of_a_terminated_actor_report_termination() {
        // The script cannot be read, so running it fails after a successful setup.
        let outcome = thread::spawn(|| {
            run_actor(
                config(),
                PathBuf::from(MISSING_SCRIPT),
                ActorLink::for_testing(true),
            )
        })
        .join()
        .unwrap();
        assert_eq!(failure(outcome), TERMINATED_MESSAGE);
    }

    #[test]
    fn setup_failures_of_a_terminated_actor_report_termination() {
        // A thread can host only one actor runtime, so creating a second one on the same
        // thread makes the actor's setup fail.
        let (live, terminated) = thread::spawn(|| {
            let _first = Runtime::new_actor(config(), ActorLink::for_testing(false)).unwrap();
            let live = run_actor(
                config(),
                PathBuf::from(MISSING_SCRIPT),
                ActorLink::for_testing(false),
            );
            let terminated = run_actor(
                config(),
                PathBuf::from(MISSING_SCRIPT),
                ActorLink::for_testing(true),
            );
            (live, terminated)
        })
        .join()
        .unwrap();
        assert!(
            failure(live).contains("Failed to create the actor runtime"),
            "a live actor must report its setup failure"
        );
        assert_eq!(failure(terminated), TERMINATED_MESSAGE);
    }
}
