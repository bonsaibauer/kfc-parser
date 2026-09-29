use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub enable_console: bool,
    #[serde(default)]
    pub use_export_flag: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_directory: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enable_console: false,
            use_export_flag: false,
            export_directory: None,
        }
    }
}

impl Config {
    pub fn load(path: impl AsRef<std::path::Path>) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let config = serde_json::from_str::<Self>(&content)?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn loading_missing_config_does_not_create_it() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "shroudforge-eml-config-{}-{nonce}.json",
            std::process::id()
        ));
        assert!(!path.exists(), "test path unexpectedly already exists");

        let config = Config::load(&path).expect("missing config should use defaults");

        assert!(!config.enable_console);
        assert!(!config.use_export_flag);
        assert!(config.export_directory.is_none());
        assert!(!path.exists(), "loading config created eml.json");
    }
}
