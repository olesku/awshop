//! Resource lists from earlier runs, per plugin, profile and region, in
//! `$XDG_CACHE_HOME/awshop/<plugin>.json`. They are shown while a fresh list loads and
//! never used for an action. Losing the file only costs a wait, so read errors count as
//! an empty cache.
use crate::{config::xdg_dir, preferences::write_private};
use awshop_plugin::{Resource, Result};
use serde::{Deserialize, Serialize};
use std::{
  fs,
  path::PathBuf,
  time::{SystemTime, UNIX_EPOCH},
};

/// Bumped when the format or a built-in plugin's columns change, since cells are matched
/// to columns by position; files with another version are ignored.
const VERSION: u32 = 1;
/// How many profile and region lists are kept per plugin, most recent first.
const ENTRIES: usize = 32;

/// One plugin's cache file.
#[derive(Default, Deserialize, Serialize)]
struct File {
  version: u32,
  entries: Vec<Entry>,
}

/// The list for one profile and region.
#[derive(Deserialize, Serialize)]
struct Entry {
  profile: String,
  region: String,
  /// Seconds since the Unix epoch.
  saved: u64,
  resources: Vec<Cached>,
}

/// A resource as the list showed it. Whether it is current is checked anew each time.
#[derive(Deserialize, Serialize)]
struct Cached {
  id: String,
  label: String,
  columns: Vec<String>,
}

/// A remembered list and how many seconds ago it was saved.
pub struct Remembered {
  pub resources: Vec<Resource>,
  pub age: u64,
}

/// `$XDG_CACHE_HOME/awshop/<plugin>.json`, or under `~/.cache`.
fn path(plugin: &str) -> PathBuf {
  xdg_dir("XDG_CACHE_HOME", ".cache").join(format!("awshop/{plugin}.json"))
}

/// The file, or an empty one when it is missing, unreadable or from another version.
fn read(plugin: &str) -> File {
  fs::read(path(plugin))
    .ok()
    .and_then(|bytes| serde_json::from_slice::<File>(&bytes).ok())
    .filter(|file| file.version == VERSION)
    .unwrap_or_default()
}

/// Seconds since the Unix epoch, or 0 when the clock is before it.
fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs())
    .unwrap_or(0)
}

/// The list last saved for this profile and region, none marked current.
pub fn load(plugin: &str, profile: &str, region: &str) -> Option<Remembered> {
  let file = read(plugin);
  let entry = file
    .entries
    .into_iter()
    .find(|entry| entry.profile == profile && entry.region == region)?;
  let resources = entry
    .resources
    .into_iter()
    .map(|cached| Resource {
      id: cached.id,
      label: cached.label,
      columns: cached.columns,
      current: false,
    })
    .collect();

  Some(Remembered {
    resources,
    age: now().saturating_sub(entry.saved),
  })
}

/// Replace the list for this profile and region. Reloads the file first so lists that
/// other shells saved are kept.
pub fn save(plugin: &str, profile: &str, region: &str, resources: &[Resource]) -> Result<()> {
  let mut file = read(plugin);
  file.version = VERSION;
  file
    .entries
    .retain(|entry| entry.profile != profile || entry.region != region);
  file.entries.insert(
    0,
    Entry {
      profile: profile.to_owned(),
      region: region.to_owned(),
      saved: now(),
      resources: resources
        .iter()
        .map(|resource| Cached {
          id: resource.id.clone(),
          label: resource.label.clone(),
          columns: resource.columns.clone(),
        })
        .collect(),
    },
  );
  file.entries.truncate(ENTRIES);

  write_private(&path(plugin), &serde_json::to_vec(&file)?)
}

/// How long ago, for the line above a remembered list.
pub fn ago(seconds: u64) -> String {
  match seconds {
    0..60 => "just now".to_owned(),
    60..3600 => format!("{} min ago", seconds / 60),
    3600..86400 => format!("{} h ago", seconds / 3600),
    86400..172800 => "1 day ago".to_owned(),
    _ => format!("{} days ago", seconds / 86400),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ages_read_naturally() {
    assert_eq!(ago(5), "just now");
    assert_eq!(ago(150), "2 min ago");
    assert_eq!(ago(7200), "2 h ago");
    assert_eq!(ago(86400), "1 day ago");
    assert_eq!(ago(86400 * 3), "3 days ago");
  }
}
