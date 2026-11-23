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
    runtime::{BundledAliases, BundledFiles, ExecutablePath},
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
        return Ok(PathBuf::from(path));
    }
    let caller = lune_std::get_calling_script_path(lua)?.ok_or_else(|| {
        LuaError::runtime(format!(
            "Cannot resolve the actor path \"{path}\" without a calling script."
        ))
    })?;
    let directory = Path::new(&caller).parent().unwrap_or_else(|| Path::new(""));
    Ok(clean_path(directory.join(path)))
}

fn run_actor(config: WorkerConfig, path: PathBuf, link: Arc<ActorLink>) -> ActorOutcome {
    async_io::block_on(async {
        let mut rt = match Runtime::new_actor(config, Arc::clone(&link)) {
            Ok(rt) => rt,
            Err(error) => {
                return ActorOutcome::from_failure(
                    &link,
                    format!("Failed to create the actor runtime. {error}"),
                );
            }
        };
        match rt.run_file(path).await {
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
