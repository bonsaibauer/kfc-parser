use std::sync::Arc;

use crate::{
    ModEnvironmentErrorReport, ModRegistry,
    alias::{Path, PathBuf},
};

struct ModEnvironmentInner {
    game_dir: PathBuf,
    cache_dir: PathBuf,
    mods_dir: PathBuf,

    registry: ModRegistry,
}

#[derive(Clone)]
pub struct ModEnvironment {
    inner: Arc<ModEnvironmentInner>,
}

impl ModEnvironment {
    pub fn load(game_dir: impl AsRef<Path>) -> Result<Self, ModEnvironmentErrorReport> {
        let game_dir = game_dir.as_ref().to_path_buf();
        let cache_dir =
            PathBuf::from_path_buf(crate::shroudforge_cache_dir(game_dir.as_std_path()))
                .expect("a UTF-8 game path joined with loader cache paths remains UTF-8");
        let mods_dir = PathBuf::from_path_buf(crate::shroudforge_directory(
            game_dir.as_std_path(),
            "mods",
            "mods",
        ))
        .expect("a UTF-8 game path joined with loader mods paths remains UTF-8");

        let registry = ModRegistry::load(&mods_dir)?;

        Ok(Self {
            inner: Arc::new(ModEnvironmentInner {
                game_dir,
                cache_dir,
                mods_dir,
                registry,
            }),
        })
    }

    pub fn game_dir(&self) -> &Path {
        &self.inner.game_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.inner.cache_dir
    }

    pub fn mods_dir(&self) -> &Path {
        &self.inner.mods_dir
    }

    pub fn mod_registry(&self) -> &ModRegistry {
        &self.inner.registry
    }
}
