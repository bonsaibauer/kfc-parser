//! Routes the two proxy DLLs to EML or ShroudForge.
//! EML remains the fallback when its standalone install has no ShroudForge config.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

static SELECTED_BACKEND: OnceLock<Backend> = OnceLock::new();

/// The selected proxy owner. EML keeps the original in-process initialization path.
/// ShroudForge receives control through its installed `winmm.dll` bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Eml,
    ShroudForge,
}

/// Reads ShroudForge's existing `modloader` setting once per process.
/// A missing config selects EML, preserving standalone EML installs. A config
/// value of `"eml"` selects EML, while ShroudForge's `"shroudforge"` default
/// selects the external ShroudForge bootstrap.
pub fn selected_backend() -> Backend {
    *SELECTED_BACKEND.get_or_init(|| {
        let Some(root) = game_root() else {
            return Backend::Eml;
        };
        let config_path = root.join("shroudforge/config/modloader-config.json");
        let Ok(bytes) = fs::read(config_path) else {
            return Backend::Eml;
        };
        let config = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
        if config
            .as_ref()
            .and_then(|value| value.get("modloader"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|backend| backend.eq_ignore_ascii_case("eml"))
        {
            Backend::Eml
        } else {
            Backend::ShroudForge
        }
    })
}

/// Result of backend dispatch. `EmlSelected` tells the proxy to run its original
/// EML initialization. `Started` means the ShroudForge bootstrap was loaded, so
/// the proxy must skip EML initialization. `Failed` aborts that proxy attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    EmlSelected,
    Started,
    Failed,
}

/// Keeps the existing EML path in the proxy and loads ShroudForge only when selected.
pub fn try_start() -> Handoff {
    match selected_backend() {
        Backend::Eml => Handoff::EmlSelected,
        Backend::ShroudForge => try_start_shroudforge(),
    }
}

/// Loads ShroudForge's installed bootstrap. This is only called for the
/// ShroudForge backend. EML initialization is handled by the proxy instead.
/// If either release file is missing or loading fails, the proxy attach fails.
/// This minimal bridge does not start the updater or change parser behavior.
#[cfg(windows)]
fn try_start_shroudforge() -> Handoff {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;

    let Some(root) = game_root() else {
        return Handoff::Failed;
    };
    let bootstrap = root.join("winmm.dll");
    let runtime = root.join("shroudforge/shroudforge-runtime.dll");
    if !bootstrap.is_file() || !runtime.is_file() {
        return Handoff::Failed;
    }
    let wide_path = bootstrap
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();

    if unsafe { LoadLibraryW(wide_path.as_ptr()) }.is_null() {
        Handoff::Failed
    } else {
        Handoff::Started
    }
}

/// Non-Windows builds cannot load the ShroudForge DLL. EML builds remain usable.
#[cfg(not(windows))]
fn try_start_shroudforge() -> Handoff {
    Handoff::Failed
}

/// The game executable's directory is the install root for both proxy backends.
fn game_root() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    executable.parent().map(Path::to_path_buf)
}
