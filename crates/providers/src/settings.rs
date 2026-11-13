use serde_json::json;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Yahoo,
    Tradier,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub provider: Provider,
    pub use_vibestrader: bool,
}

impl Settings {
    fn from_json(value: &serde_json::Value) -> Self {
        Self {
            provider: if value["provider"] == "tradier" {
                Provider::Tradier
            } else {
                Provider::Yahoo
            },
            use_vibestrader: cfg!(feature = "vibestrader-credential")
                && value["use_vibestrader"].as_bool().unwrap_or(false),
        }
    }
    fn path() -> Result<PathBuf, &'static str> {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or("Local application data folder is unavailable")?;
        let filename = if cfg!(feature = "vibestrader-credential") {
            "settings.json"
        } else {
            "settings-community.json"
        };
        Ok(PathBuf::from(base).join("TradeCraft").join(filename))
    }
    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .map(|value| Self::from_json(&value))
            .unwrap_or_default()
    }
    pub fn save(self) -> Result<(), &'static str> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().ok_or("Invalid settings folder")?)
            .map_err(|_| "Could not create settings folder")?;
        let contents = json!({"provider": if self.provider == Provider::Tradier { "tradier" } else { "yahoo" }, "use_vibestrader": self.use_vibestrader});
        std::fs::write(path, contents.to_string()).map_err(|_| "Could not save settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_setting_is_only_enabled_in_personal_builds() {
        let settings = Settings::from_json(&json!({"provider":"tradier", "use_vibestrader":true}));
        assert_eq!(settings.provider, Provider::Tradier);
        assert_eq!(
            settings.use_vibestrader,
            cfg!(feature = "vibestrader-credential")
        );
    }

    #[test]
    fn builds_use_separate_settings_and_default_to_yahoo() {
        let expected = if cfg!(feature = "vibestrader-credential") {
            "settings.json"
        } else {
            "settings-community.json"
        };
        assert_eq!(Settings::path().unwrap().file_name().unwrap(), expected);
        assert_eq!(Settings::default().provider, Provider::Yahoo);
    }
}
