use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub editor: Option<String>,
    /// Argv template for an `@video` action, sharing yy's placeholder shape so
    /// there is one mental model across the ecosystem:
    /// `player = ["yclippy", "play", "{url}", "--at", "{seconds}"]`.
    ///
    /// A template without `{seconds}` gets the timestamp rebuilt into `{url}`,
    /// so `["xdg-open", "{url}"]` still lands at the right moment.
    pub player: Option<Vec<String>>,
    pub desired_retention: f32,
    pub new_cards_per_day: usize,
    pub max_reviews_per_day: usize,
    pub review_order: ReviewOrder,
    pub bury_siblings: bool,
    pub reindex_interval_ms: u64,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewOrder {
    Due,
    Random,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            editor: None,
            player: None,
            desired_retention: 0.9,
            new_cards_per_day: 20,
            max_reviews_per_day: 200,
            review_order: ReviewOrder::Due,
            bury_siblings: true,
            reindex_interval_ms: 1_000,
        }
    }
}

impl Config {
    pub fn load(vault: &Path) -> Result<Self> {
        let path = vault.join(".notes/config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let config: Self = toml::from_str(&fs::read_to_string(&path)?)
            .with_context(|| format!("invalid configuration in {}", path.display()))?;
        if !(0.7..=0.99).contains(&config.desired_retention) {
            anyhow::bail!("desired_retention must be between 0.70 and 0.99");
        }
        if config.new_cards_per_day == 0 || config.max_reviews_per_day == 0 {
            anyhow::bail!("review limits must be greater than zero");
        }
        Ok(config)
    }

    pub fn save(&self, vault: &Path) -> Result<()> {
        let path = vault.join(".notes/config.toml");
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("writing configuration to {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn saves_and_loads_review_options() {
        let dir = tempdir().unwrap();
        let config = Config {
            desired_retention: 0.95,
            new_cards_per_day: 12,
            max_reviews_per_day: 80,
            review_order: ReviewOrder::Random,
            bury_siblings: false,
            ..Config::default()
        };
        config.save(dir.path()).unwrap();

        let loaded = Config::load(dir.path()).unwrap();
        assert_eq!(loaded.desired_retention, 0.95);
        assert_eq!(loaded.new_cards_per_day, 12);
        assert_eq!(loaded.max_reviews_per_day, 80);
        assert_eq!(loaded.review_order, ReviewOrder::Random);
        assert!(!loaded.bury_siblings);
    }
}
