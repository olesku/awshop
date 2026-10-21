//! Commands that work on the AWS config itself: `awshop add` and `awshop --doctor`.
use crate::aws::{self, AwsConfig, Profile};
use awshop_plugin::Result;
use std::{
  collections::{BTreeMap, HashSet},
  io::{self, Write},
  process::Command,
};

/// Create a profile with `aws configure sso`, which writes `~/.aws/config` itself.
pub fn add(config: &AwsConfig, args: &[String]) -> Result<()> {
  if args.len() > 1 {
    return Err("Usage: awshop add [profile-name]".into());
  }

  let profile_name = if let Some(name) = args.first() {
    name.trim().to_owned()
  } else {
    eprint!("Name for the new AWS profile: ");
    io::stderr().flush()?;
    let mut name = String::new();
    io::stdin().read_line(&mut name)?;
    name.trim().to_owned()
  };

  if profile_name.is_empty() {
    return Err("Profile name cannot be empty.".into());
  }
  if config.profile(&profile_name).is_some() {
    return Err(format!(
            "AWS profile '{profile_name}' already exists. Choose another name or update it with `aws configure sso --profile {profile_name}`."
        )
        .into());
  }

  let aws_cli = aws::find_aws_cli()?;
  if !config.sessions.is_empty() {
    eprintln!(
      "Configured SSO sessions: {}",
      config
        .sessions
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ")
    );
  }

  eprintln!(
        "Starting the AWS CLI SSO setup for profile '{profile_name}'. Use the same SSO session name as your other profiles to share the login."
    );
  let status = Command::new(aws_cli)
    .args(["configure", "sso", "--profile", &profile_name])
    .status()?;
  if !status.success() {
    return Err(format!("AWS CLI SSO setup failed ({status}).").into());
  }

  eprintln!("Added AWS profile '{profile_name}'. Run `awshop` to select it.");
  Ok(())
}

/// Summarize the SSO setup without printing account IDs or start URLs.
pub fn diagnose(config: &AwsConfig) {
  // Start URLs are compared without trailing `/` or `#`.
  let normalize = |url: &str| url.trim_end_matches(['/', '#']).to_owned();
  let sso_count = config
    .profiles
    .iter()
    .filter(|profile| profile.session.is_some() || profile.legacy_sso)
    .count();
  let legacy: Vec<&Profile> = config.profiles.iter().filter(|profile| profile.legacy_sso).collect();
  let linked: HashSet<&str> = config
    .profiles
    .iter()
    .filter_map(|profile| profile.session.as_deref())
    .collect();
  let unused: Vec<&str> = config
    .sessions
    .keys()
    .map(String::as_str)
    .filter(|session| !linked.contains(session))
    .collect();

  println!("AWS config: {}", aws::config_path().display());
  println!("Profiles: {}", config.profiles.len());
  println!(
    "SSO profiles: {sso_count} ({} shared-session, {} legacy)",
    sso_count - legacy.len(),
    legacy.len()
  );
  println!("Configured SSO sessions: {}", config.sessions.len());
  if !unused.is_empty() {
    println!("Unused SSO sessions: {}", unused.join(", "));
  }

  for profile in &config.profiles {
    if profile
      .session
      .as_ref()
      .is_some_and(|session| !config.sessions.contains_key(session))
    {
      println!("Profile '{}' references an undefined SSO session.", profile.name);
    }
  }

  // Legacy profiles with the same start URL could share one sso-session.
  let mut legacy_groups: BTreeMap<String, Vec<&str>> = BTreeMap::new();
  for profile in &legacy {
    if let Some(url) = &profile.sso_start_url {
      legacy_groups.entry(normalize(url)).or_default().push(&profile.name);
    }
  }
  if !legacy_groups.is_empty() {
    println!("Legacy SSO start URL groups: {}", legacy_groups.len());
    if legacy_groups.len() > 1 {
      for (index, profiles) in legacy_groups.values().enumerate() {
        println!("  group {}: {}", index + 1, profiles.join(", "));
      }
    }
    for (name, session) in &config.sessions {
      if session
        .get("sso_start_url")
        .is_some_and(|url| !legacy_groups.contains_key(&normalize(url)))
      {
        println!("SSO session '{name}' has a start URL that does not match any legacy SSO profile.");
      }
    }
  }

  for profile in &config.profiles {
    if let Some(source) = &profile.source_profile {
      println!("Role chain: {} uses source profile {source}", profile.name);
    }
  }
}
