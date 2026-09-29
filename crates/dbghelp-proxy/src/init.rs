use std::{
    os::windows::ffi::OsStrExt,
    path::PathBuf,
};

use camino::{Utf8Path, Utf8PathBuf};
use mod_loader::{lua::{self, RunArgs, RunOptions}, runtime, Config, ModEnvironment};

use crate::{log::error, logging};

pub fn init(config: Config) {
    // ShroudForge owns the runtime when installed beside the EML proxy. Hand
    // off to its existing game-root bootstrap instead of starting EML twice.
    if let Some(bootstrap) = shroudforge_bootstrap_path() {
        let wide_path = bootstrap
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if unsafe {
            windows::Win32::System::LibraryLoader::LoadLibraryW(windows::core::PCWSTR(
                wide_path.as_ptr(),
            ))
        }
        .is_ok()
        {
            return;
        }
        let message = format!("ShroudForge winmm bootstrap could not be loaded from {bootstrap:?}");
        if !mod_loader::append_shroudforge_diagnostic('E', "native-proxy", &message) {
            error!("{message}");
        }
    }

    logging::setup();

    let game_directory = std::env::current_exe()
        .expect("Failed to get game executable path")
        .parent()
        .expect("Game executable has no parent directory")
        .to_path_buf();
    let game_directory = Utf8PathBuf::from_path_buf(game_directory)
        .expect("Game directory path is not valid UTF-8");

    let env = match ModEnvironment::load(&game_directory) {
        Ok(env) => env,
        Err(e) => {
            if let Some(error) = e.error {
                error!(
                    error = %error,
                    "Error loading mod environment"
                );
                panic!("Error loading mod environment: {error}");
            }

            let mut report = String::from("Errors loading some mods:");

            for mod_error in e.mods {
                if let Some(id) = &mod_error.id {
                    report += &format!(
                        "\n  In mod '{}' at {}: {}",
                        id,
                        mod_error.path,
                        mod_error.error
                    );
                } else {
                    report += &format!(
                        "\n  In mod at {}: {}",
                        mod_error.path,
                        mod_error.error
                    );
                }
            }

            error!(
                report = %report,
                "Error loading mod environment"
            );
            panic!("{report}");
        }
    };

    let result = lua::run(
        &env,
        RunArgs {
            file_name: get_file_name(&game_directory),
            options: RunOptions {
                patch: true,
                export: config.use_export_flag,
                export_dir: config.export_directory.map(Utf8PathBuf::from),
                runtime: true,
                ..Default::default()
            },
        },
    );

    let result = match result {
        Ok(result) => result,
        Err(e) => {
            error!(
                error = %e,
                "Error running mod loader"
            );
            panic!("Error running mod loader: {e}");
        }
    };

    runtime::loader_attach(
        &env,
        runtime::RuntimeOptions {
            dlls: result
                .dlls
                .into_iter()
                .map(Utf8PathBuf::into_std_path_buf)
                .collect(),
        },
    ).expect("Failed to attach runtime loader");
}

pub(crate) fn shroudforge_bootstrap_available() -> bool {
    shroudforge_bootstrap_path().is_some()
}

fn shroudforge_bootstrap_path() -> Option<PathBuf> {
    // Launchers such as Steam may set an unrelated working directory. Resolve
    // ShroudForge beside the running game executable instead.
    let game_directory = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let runtime = game_directory.join("shroudforge/shroudforge-runtime.dll");
    let bootstrap = game_directory.join("winmm.dll");
    (runtime.is_file() && bootstrap.is_file()).then_some(bootstrap)
}

pub fn deinit() {
    runtime::loader_detach()
        .expect("Failed to detach runtime loader");
}

fn get_file_name(
    game_dir: &Utf8Path,
) -> String {
    if game_dir.join("enshrouded.kfc").exists() || game_dir.join("enshrouded.exe").exists() {
        "enshrouded".into()
    } else if game_dir.join("enshrouded_server.kfc").exists() || game_dir.join("enshrouded_server.exe").exists() {
        "enshrouded_server".into()
    } else {
        // try to find file with .kfc or .exe extension
        for file in std::fs::read_dir(game_dir).into_iter().flatten().flatten() {
            if let Some(ext) = file.path().extension() {
                if ext == "kfc" || ext == "exe" {
                    let file = file.path();
                    let file_name = file.file_stem()
                        .and_then(|s| s.to_str());

                    if let Some(file_name) = file_name {
                        return file_name.into();
                    }
                }
            }
        }

        error!(
            game_dir = %game_dir,
            "Could not find required game files"
        );
        panic!("Could not find required game files in {game_dir}");
    }
}
