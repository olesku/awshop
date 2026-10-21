//! User configuration in `$XDG_CONFIG_HOME/awshop/config.toml`. awshop only reads it;
//! what it remembers by itself goes to the state file (see `preferences`).
use crate::preferences::Choices;
use awshop_plugin::Result;
use serde::Deserialize;
use std::{collections::BTreeMap, env, fs, io, path::PathBuf};

/// The whole file: `[sso]`, then one table per plugin, keyed by plugin name.
#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Config {
  pub sso: SsoConfig,
  #[serde(flatten)]
  plugins: BTreeMap<String, PluginConfig>,
}

/// Settings in the `[sso]` table.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SsoConfig {
  /// Keep profiles for the accounts the SSO portal grants in the AWS config.
  pub sync: bool,
}

impl Default for SsoConfig {
  fn default() -> Self {
    Self { sync: true }
  }
}

/// Settings in a `[<plugin>]` table. Unknown keys are errors, so typos are reported.
#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PluginConfig {
  /// Default scope for plugins with shell sessions.
  pub scope: Option<Scope>,
  /// Named values for saved-choice fields, by collection (e.g. `iam-roles`).
  pub choices: Choices,
}

/// Where a session plugin applies its setup by default.
#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
  /// A temporary session in this shell.
  Shell,
  /// The saved configuration file.
  Saved,
}

impl Config {
  /// The parsed file, or defaults when it does not exist.
  pub fn load() -> Result<Self> {
    let path = xdg_dir("XDG_CONFIG_HOME", ".config").join("awshop/config.toml");
    match fs::read_to_string(&path) {
      Ok(text) => Self::parse(&text).map_err(|error| format!("Invalid {}: {error}", path.display()).into()),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
      Err(error) => Err(error.into()),
    }
  }

  /// The settings in config file text.
  fn parse(text: &str) -> std::result::Result<Self, toml::de::Error> {
    toml::from_str(text)
  }

  /// The table for one plugin, or defaults.
  pub fn plugin(&self, name: &str) -> PluginConfig {
    self.plugins.get(name).cloned().unwrap_or_default()
  }
}

/// An XDG base directory from `variable`, or `~/<fallback>` when it is unset or empty.
pub fn xdg_dir(variable: &str, fallback: &str) -> PathBuf {
  env::var_os(variable)
    .filter(|value| !value.is_empty())
    .map(PathBuf::from)
    .unwrap_or_else(|| awshop_plugin::home().join(fallback))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reads_plugin_tables_and_rejects_typos() {
    let config = Config::parse(
      "[eks]\nscope = \"saved\"\n\n[eks.choices.iam-roles]\nreader = \"arn:aws:iam::123456789012:role/reader\"\n",
    )
    .unwrap();
    let eks = config.plugin("eks");
    assert!(eks.scope == Some(Scope::Saved));
    assert_eq!(
      eks.choices["iam-roles"]["reader"],
      "arn:aws:iam::123456789012:role/reader"
    );

    assert!(config.plugin("other").scope.is_none());
    assert!(Config::parse("[eks]\nscop = \"saved\"\n").is_err());
    assert!(Config::parse("[eks]\nscope = \"temporary\"\n").is_err());

    assert!(config.sso.sync);
    assert!(!Config::parse("[sso]\nsync = false\n").unwrap().sso.sync);
    assert!(Config::parse("[sso]\nsink = false\n").is_err());
  }
}
