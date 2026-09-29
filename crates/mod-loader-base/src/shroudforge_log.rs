use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::Instant,
};

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Stages an early native proxy diagnostic for the ShroudForge bootstrap log.
/// Returns false when this is an EML-only installation.
pub fn append_shroudforge_diagnostic(level: char, source: &str, message: &str) -> bool {
    let Ok(root) = std::env::current_dir() else {
        return false;
    };
    if !root.join("shroudforge/shroudforge-runtime.dll").is_file()
        || !root.join("winmm.dll").is_file()
    {
        return false;
    }

    if !allows(&root, level) {
        return true;
    }

    let elapsed = STARTED.get_or_init(Instant::now).elapsed();
    let seconds = elapsed.as_secs();
    let source = source
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace(']', "_");
    let message = message.replace('\r', "\\r").replace('\n', "\\n");
    let line = format!(
        "[{level} {:02}:{:02}:{:02},{:03}] [{source}] {message}\n",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60,
        elapsed.subsec_millis()
    );

    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_ABANDONED, WAIT_OBJECT_0},
            System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject},
        };

        let mutex_name: Vec<u16> = "Local\\ShroudForgeLog"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr()) };
        if mutex.is_null() {
            let _ = append_line(&pending_path(&root), line.as_bytes());
            return true;
        }
        let wait = unsafe { WaitForSingleObject(mutex, u32::MAX) };
        if wait != WAIT_OBJECT_0 && wait != WAIT_ABANDONED {
            unsafe { CloseHandle(mutex) };
            let _ = append_line(&pending_path(&root), line.as_bytes());
            return true;
        }

        let path = pending_path(&root);
        let result = append_line(&path, line.as_bytes());
        unsafe {
            ReleaseMutex(mutex);
            CloseHandle(mutex);
        }
        let _ = result;
    }

    #[cfg(not(windows))]
    let _ = (root, source, line);

    true
}

/// Resolves a configured ShroudForge data directory from loader settings.
pub fn shroudforge_directory(root: &Path, key: &str, default: &str) -> PathBuf {
    fs::read(root.join("shroudforge/config/modloader-config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .pointer(&format!("/paths/{key}"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .unwrap_or_else(|| root.join(default))
}

/// Resolves the shared ShroudForge and EML cache folder from loader settings.
pub fn shroudforge_cache_dir(root: &Path) -> PathBuf {
    shroudforge_directory(root, "cache", "shroudforge/cache")
}

fn allows(root: &Path, level: char) -> bool {
    let minimum = fs::read(root.join("shroudforge/config/modloader-config.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .pointer("/logging/minimumLevel")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "INFO".to_owned());
    let minimum = match minimum.to_ascii_uppercase().as_str() {
        "TRACE" | "ALL" => 0,
        "DEBUG" => 1,
        "INFO" => 2,
        "WARN" | "WARNING" => 3,
        "ERROR" => 4,
        _ => 2,
    };
    let level = match level {
        'T' => 0,
        'D' => 1,
        'I' => 2,
        'W' => 3,
        'E' => 4,
        _ => 2,
    };
    level >= minimum
}

fn pending_path(root: &Path) -> std::path::PathBuf {
    shroudforge_directory(root, "logs", "shroudforge/logs").join("native-proxy.pending")
}

fn append_line(path: &Path, line: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line)?;
    file.flush()
}
