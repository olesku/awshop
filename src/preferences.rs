//! What awshop remembers between runs: plugin settings, named values and recent
//! profiles. This is state, not configuration; it is written only after a successful
//! action and lives in `$XDG_STATE_HOME`. Hand-written settings belong in `config.toml`.
use crate::config::xdg_dir;
use awshop_plugin::{Result, Settings};
use serde::{Deserialize, Serialize};
use std::{
  collections::BTreeMap,
  fs, io,
  io::Write,
  path::{Path, PathBuf},
};

/// Settings by profile, then resource ID.
type ResourceSettings = BTreeMap<String, BTreeMap<String, Settings>>;
/// Named values by collection, then name.
pub type Choices = BTreeMap<String, BTreeMap<String, String>>;

/// The preferences file. Both maps are keyed by plugin name first.
#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Preferences {
  pub plugins: BTreeMap<String, ResourceSettings>,
  pub choices: BTreeMap<String, Choices>,
  /// Profiles set through the selector, most recent first. The selector lists them first.
  pub recent_profiles: Vec<String>,
}

/// How many recent profiles are kept.
const RECENT_PROFILES: usize = 5;

/// `$XDG_STATE_HOME/awshop/preferences.json`, or `~/.local/state/awshop/preferences.json`.
fn path() -> PathBuf {
  xdg_dir("XDG_STATE_HOME", ".local/state").join("awshop/preferences.json")
}

/// Where earlier versions kept the file. It is read until the first save moves it.
fn legacy_path() -> PathBuf {
  xdg_dir("XDG_CONFIG_HOME", ".config").join("awshop/preferences.json")
}

/// Store a successful setup and its named choices. Reloads the file first so
/// changes from other shells since this run started are kept.
pub fn remember(plugin: &str, profile: &str, resource: &str, settings: Settings, choices: &Choices) -> Result<()> {
  let mut latest = Preferences::load()?;
  latest
    .plugins
    .entry(plugin.into())
    .or_default()
    .entry(profile.into())
    .or_default()
    .insert(resource.into(), settings);

  let collections = latest.choices.entry(plugin.into()).or_default();
  for (collection, values) in choices {
    collections
      .entry(collection.clone())
      .or_default()
      .extend(values.clone());
  }

  store(&latest)
}

/// Put `profile` first among the recent profiles.
pub fn remember_profile(profile: &str) -> Result<()> {
  let mut latest = Preferences::load()?;
  latest.recent_profiles.retain(|name| name != profile);
  latest.recent_profiles.insert(0, profile.to_owned());
  latest.recent_profiles.truncate(RECENT_PROFILES);
  store(&latest)
}

/// Save to the state file and remove the legacy one, which the save replaces.
fn store(preferences: &Preferences) -> Result<()> {
  preferences.save(&path())?;
  match fs::remove_file(legacy_path()) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.into()),
    _ => Ok(()),
  }
}

impl Preferences {
  /// The state file, the legacy file, or an empty set when neither exists yet.
  pub fn load() -> Result<Self> {
    for path in [path(), legacy_path()] {
      match fs::read(&path) {
        Ok(bytes) => {
          return serde_json::from_slice(&bytes)
            .map_err(|error| format!("Cannot read preferences {}: {error}", path.display()).into())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
      }
    }

    Ok(Self::default())
  }

  /// Saved settings for one plugin, profile and resource, or none.
  pub fn settings(&self, plugin: &str, profile: &str, resource: &str) -> Settings {
    self
      .plugins
      .get(plugin)
      .and_then(|profiles| profiles.get(profile))
      .and_then(|resources| resources.get(resource))
      .cloned()
      .unwrap_or_default()
  }

  /// Write the file atomically and private to the user.
  fn save(&self, path: &Path) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(self)?;
    bytes.push(b'\n');
    write_private(path, &bytes)
  }
}

/// Write atomically: a private temporary file next to `path`, then rename over it.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
  let parent = path.parent().ok_or("State path has no parent directory.")?;
  let name = path.file_name().ok_or("State path has no file name.")?;
  fs::create_dir_all(parent)?;

  let temporary = parent.join(format!(".{}-{}.tmp", name.to_string_lossy(), std::process::id()));
  let mut options = fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }

  let mut file = options.open(&temporary)?;
  let result = (|| -> Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
  })();

  if result.is_err() {
    let _ = fs::remove_file(&temporary);
  }
  result
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn settings_are_scoped_by_plugin_profile_and_cluster_identity() {
    let mut preferences = Preferences::default();
    preferences
      .plugins
      .entry("eks".into())
      .or_default()
      .entry("dev".into())
      .or_default()
      .insert("arn:cluster-a".into(), [("role_arn".into(), "reader".into())].into());

    assert_eq!(
      preferences.settings("eks", "dev", "arn:cluster-a")["role_arn"],
      "reader"
    );
    assert!(preferences.settings("eks", "prod", "arn:cluster-a").is_empty());
    assert!(preferences.settings("eks", "dev", "arn:cluster-b").is_empty());
    assert!(preferences.settings("another", "dev", "arn:cluster-a").is_empty());
  }
}
