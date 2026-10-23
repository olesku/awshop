//! AWS CLI configuration: profiles, SSO sessions and login checks.
use crate::config::Detail;
use awshop_plugin::{Account, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::{env, fs, io};

/// One profile from `~/.aws/config` or `~/.aws/credentials`.
#[derive(Clone, Debug, Default)]
pub struct Profile {
  pub name: String,
  pub region: Option<String>,
  pub role: Option<String>,
  pub account: Option<String>,
  pub session: Option<String>,
  pub sso_start_url: Option<String>,
  /// SSO settings directly in the profile instead of a shared `sso-session`.
  pub legacy_sso: bool,
  pub source_profile: Option<String>,
}

impl Profile {
  /// A profile with only a name, such as one from the credentials file.
  fn named(name: impl Into<String>) -> Self {
    Self {
      name: name.into(),
      ..Self::default()
    }
  }

  /// A profile from its config file section.
  fn from_section(name: &str, values: &HashMap<String, String>) -> Self {
    Self {
      name: name.to_owned(),
      region: values.get("region").cloned(),
      role: values.get("sso_role_name").cloned(),
      account: values.get("sso_account_id").cloned(),
      session: values.get("sso_session").cloned(),
      sso_start_url: values.get("sso_start_url").cloned(),
      legacy_sso: values.contains_key("sso_start_url") && !values.contains_key("sso_session"),
      source_profile: values.get("source_profile").cloned(),
    }
  }

  /// The chosen fields for selector rows, in order; empty where the profile has none.
  pub fn cells(&self, fields: &[Detail]) -> Vec<String> {
    fields
      .iter()
      .map(|field| match field {
        Detail::Login => self.login(),
        Detail::Account => self.account.clone().unwrap_or_default(),
        Detail::Role => self.role.clone().unwrap_or_default(),
        Detail::Region => self.region.clone().unwrap_or_default(),
      })
      .collect()
  }

  /// How the profile logs in, in a word or two; empty for plain profiles.
  fn login(&self) -> String {
    if let Some(session) = &self.session {
      format!("SSO {session}")
    } else if self.legacy_sso {
      "SSO legacy".to_owned()
    } else if let Some(source) = &self.source_profile {
      format!("via {source}")
    } else {
      String::new()
    }
  }
}

/// The result of a login check.
pub enum Login {
  /// The credentials work; the account, when the check could tell it.
  Valid(Option<Account>),
  /// The profile uses SSO and needs `aws sso login`.
  Needed,
}

/// Profiles (sorted by name) and `[sso-session]` sections.
#[derive(Clone, Default)]
pub struct AwsConfig {
  pub profiles: Vec<Profile>,
  pub sessions: BTreeMap<String, HashMap<String, String>>,
}

impl AwsConfig {
  /// Read the config file and add profiles that exist only in the credentials file.
  pub fn load() -> io::Result<Self> {
    let mut config = Self::parse(&read_optional(&config_path())?);

    for name in sections(&read_optional(&credentials_path())?).into_keys() {
      if config.profile(&name).is_none() {
        config.profiles.push(Profile::named(name));
      }
    }

    config.profiles.sort_by_key(|profile| profile.name.to_lowercase());
    Ok(config)
  }

  /// Profiles and sessions in config file text, without the credentials file.
  pub fn parse(contents: &str) -> Self {
    let mut config = Self::default();
    for (section, values) in sections(contents) {
      if section == "default" {
        config.profiles.push(Profile::from_section(&section, &values));
      } else if let Some(name) = section.strip_prefix("profile ") {
        config.profiles.push(Profile::from_section(name, &values));
      } else if let Some(name) = section.strip_prefix("sso-session ") {
        config.sessions.insert(name.to_owned(), values);
      }
    }

    config
  }

  /// The profile named `name`, if any.
  pub fn profile(&self, name: &str) -> Option<&Profile> {
    self.profiles.iter().find(|profile| profile.name == name)
  }

  /// Follow `source_profile` links to the profile that holds the SSO settings.
  /// Returns None for non-SSO chains and for cycles.
  fn sso_login_profile<'a>(&'a self, profile: &'a Profile) -> Option<&'a Profile> {
    let mut current = profile;
    let mut visited = HashSet::new();
    while visited.insert(current.name.as_str()) {
      if current.session.is_some() || current.legacy_sso {
        return Some(current);
      }
      current = self.profile(current.source_profile.as_deref()?)?;
    }
    None
  }

  /// Whether the profile's SSO credentials work, without logging in, so it can run in
  /// the background. Other profile types are left to the AWS CLI and count as valid.
  pub fn check_login(&self, profile: &Profile) -> Result<Login> {
    if self.sso_login_profile(profile).is_none() {
      return Ok(Login::Valid(None));
    }

    let identity = Command::new(find_aws_cli()?)
      .args([
        "sts",
        "get-caller-identity",
        "--profile",
        &profile.name,
        "--output",
        "json",
      ])
      .stderr(Stdio::null())
      .output()?;
    if !identity.status.success() {
      return Ok(Login::Needed);
    }

    let account = serde_json::from_slice(&identity.stdout)
      .ok()
      .and_then(|identity| Account::from_identity(&identity).ok());
    Ok(Login::Valid(account))
  }

  /// Run `aws sso login` when the profile uses SSO and its credentials do not work.
  /// Returns the account when the check found it, so plugins don't look it up again.
  pub fn ensure_login(&self, profile: &Profile) -> Result<Option<Account>> {
    if let Login::Valid(account) = self.check_login(profile)? {
      return Ok(account);
    }

    let login_profile = self.sso_login_profile(profile).ok_or("The profile has no SSO login.")?;
    eprintln!(
      "No valid AWS session for '{}'; starting SSO login via '{}'.",
      profile.name, login_profile.name
    );
    let login = Command::new(find_aws_cli()?)
      .args(["sso", "login", "--profile", &login_profile.name])
      // Stdout is reserved for machine-readable output, so the dialog goes to stderr.
      .stdout(io::stderr())
      .status()?;
    if !login.success() {
      return Err(format!("AWS SSO login failed for profile '{}' ({login}).", login_profile.name).into());
    }
    Ok(None)
  }
}

/// Parse INI-style sections into lowercase keys. Comments and stray lines are ignored.
fn sections(contents: &str) -> BTreeMap<String, HashMap<String, String>> {
  let mut sections: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
  let mut current = None;

  for line in contents.lines().map(str::trim) {
    if line.is_empty() || line.starts_with(['#', ';']) {
      continue;
    }

    if let Some(name) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) {
      let name = name.trim().to_owned();
      sections.entry(name.clone()).or_default();
      current = Some(name);
    } else if let Some((key, value)) = line.split_once('=') {
      if let Some(values) = current.as_ref().and_then(|section| sections.get_mut(section)) {
        values.insert(key.trim().to_lowercase(), value.trim().to_owned());
      }
    }
  }

  sections
}

/// File contents, or an empty string when the file does not exist.
fn read_optional(path: &Path) -> io::Result<String> {
  match fs::read_to_string(path) {
    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
    result => result,
  }
}

/// `AWS_CONFIG_FILE`, or `~/.aws/config`, as the AWS CLI resolves it.
pub fn config_path() -> PathBuf {
  aws_file("AWS_CONFIG_FILE", "config")
}

/// `AWS_SHARED_CREDENTIALS_FILE`, or `~/.aws/credentials`.
fn credentials_path() -> PathBuf {
  aws_file("AWS_SHARED_CREDENTIALS_FILE", "credentials")
}

/// The file in `variable`, or `~/.aws/<name>`.
fn aws_file(variable: &str, name: &str) -> PathBuf {
  env::var_os(variable)
    .map(PathBuf::from)
    .unwrap_or_else(|| awshop_plugin::home().join(".aws").join(name))
}

/// The profile the AWS CLI would use in this shell.
pub fn active_profile() -> String {
  env::var("AWS_PROFILE")
    .or_else(|_| env::var("AWS_DEFAULT_PROFILE"))
    .unwrap_or_else(|_| "default".to_owned())
}

/// Resolve an executable in PATH, as the shell would.
pub fn find_executable(name: &str) -> Option<PathBuf> {
  env::split_paths(&env::var_os("PATH")?)
    .map(|directory| directory.join(name))
    .find(|candidate| is_executable(candidate))
}

/// The `aws` executable, or an error with install advice.
pub fn find_aws_cli() -> Result<PathBuf> {
  find_executable("aws").ok_or_else(|| {
    "AWS CLI ('aws') was not found in PATH. Install AWS CLI v2 and ensure its executable is in PATH.".into()
  })
}

/// A regular file with an execute bit.
fn is_executable(path: &Path) -> bool {
  use std::os::unix::fs::PermissionsExt;
  fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::Config;

  #[test]
  fn parses_profiles_sessions_and_role_chains() {
    let config = AwsConfig::parse(
      "# comment\n[default]\nregion = eu-north-1\n[profile dev]\nsso_session = corp\nsso_account_id = 123456789012\nsso_role_name = Admin\n\
             [profile legacy]\nsso_start_url = https://example\n[profile chained]\nsource_profile = dev\n\
             [profile loop-a]\nsource_profile = loop-b\n[profile loop-b]\nsource_profile = loop-a\n\
             [sso-session corp]\nsso_region = eu-north-1\n",
    );

    assert_eq!(config.profile("default").unwrap().region.as_deref(), Some("eu-north-1"));
    assert!(config.sessions.contains_key("corp"));
    assert!(config.profile("legacy").unwrap().legacy_sso);
    let dev = config.profile("dev").unwrap();
    assert_eq!(
      dev.cells(&Config::default().selector.details),
      ["SSO corp", "123456789012", "Admin", ""]
    );
    assert_eq!(dev.cells(&[Detail::Role]), ["Admin"]);
    assert_eq!(config.profile("chained").unwrap().cells(&[Detail::Login]), ["via dev"]);

    let login = |name| {
      config
        .sso_login_profile(config.profile(name).unwrap())
        .map(|p| p.name.as_str())
    };
    assert_eq!(login("chained"), Some("dev"));
    assert_eq!(login("loop-a"), None);
    assert_eq!(login("default"), None);
  }
}
