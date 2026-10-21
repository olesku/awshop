//! Profiles from AWS IAM Identity Center. For every `[sso-session]`, awshop asks the SSO
//! portal which accounts and roles you have, and keeps a profile for each in a marked
//! block at the end of the AWS config. Everything outside the block is left alone and
//! wins: a grant that one of your own profiles already covers gets no generated profile.
//! The portal is called over HTTPS from this process, so the token never leaves it.
use crate::aws::{self, AwsConfig};
use awshop_plugin::Result;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::Value;
use std::{
  collections::{BTreeMap, HashSet},
  env, fs,
  io::{self, Write},
  os::unix::fs::{OpenOptionsExt, PermissionsExt},
  path::Path,
  thread,
  time::Duration,
};
use ureq::Agent;

/// The first line of awshop's block in the AWS config.
const BEGIN: &str = "# BEGIN awshop: profiles from SSO, kept up to date by awshop. Edits here are replaced.";
/// The last line of the block.
const END: &str = "# END awshop";
/// Portal requests that run at once while roles are listed.
const PARALLEL: usize = 8;

/// An account and role that the SSO portal grants.
struct Grant {
  session: String,
  start_url: String,
  region: String,
  account: String,
  account_name: String,
  role: String,
}

/// What a sync changed: the profiles it added and removed.
pub struct Changes {
  pub added: Vec<String>,
  pub removed: Vec<String>,
}

/// Bring the generated profiles in line with the portal. Each session is synced on its
/// own: one without a valid login keeps the profiles it had. None when no session could
/// be listed, in which case nothing changes.
pub fn sync() -> Result<Option<Changes>> {
  let path = aws::config_path();
  let read = || -> Result<String> {
    fs::read_to_string(&path).map_err(|error| match error.kind() {
      io::ErrorKind::NotFound => format!("No AWS config at {}.", path.display()).into(),
      _ => error.into(),
    })
  };

  let text = read()?;
  let (before, _, after) = split(&text, &path)?;
  let own = AwsConfig::parse(&format!("{before}{after}"));
  if own.sessions.is_empty() {
    return Err(format!("No [sso-session] in {} to list accounts from.", path.display()).into());
  }

  let agent: Agent = Agent::config_builder()
    .timeout_global(Some(Duration::from_secs(20)))
    .build()
    .into();

  let mut grants = Vec::new();
  let mut skipped = HashSet::new();
  for (session, values) in &own.sessions {
    // A session that can't be listed keeps the profiles it has.
    let (Some(start_url), Some(region)) = (values.get("sso_start_url"), values.get("sso_region")) else {
      skipped.insert(session.clone());
      continue;
    };

    let granted = match token(session)? {
      Some(token) => fetch(&agent, session, start_url, region, &token)?,
      None => None,
    };

    match granted {
      Some(granted) => grants.extend(granted),
      None => {
        skipped.insert(session.clone());
      }
    }
  }

  if skipped.len() == own.sessions.len() {
    return Ok(None);
  }

  // Listing took a while: build on the file as it is now, so edits made meanwhile stay.
  let text = read()?;
  let (before, block, after) = split(&text, &path)?;
  let own = AwsConfig::parse(&format!("{before}{after}"));
  let kept = entries(block)
    .into_iter()
    .filter(|(_, (session, _))| skipped.contains(session.as_str()))
    .map(|(name, (_, text))| (name, text))
    .collect();
  let rendered = render(&own, &grants, kept);

  let names = |text: &str| -> HashSet<String> {
    AwsConfig::parse(text)
      .profiles
      .into_iter()
      .map(|profile| profile.name)
      .collect()
  };
  let (old, new) = (names(block), names(&rendered));
  let mut changes = Changes {
    added: new.difference(&old).cloned().collect(),
    removed: old.difference(&new).cloned().collect(),
  };
  changes.added.sort();
  changes.removed.sort();

  if rendered != block {
    let text = if block.is_empty() {
      // The user's text stays as it is, with one blank line before the new block.
      let separator = if before.is_empty() || before.ends_with("\n\n") {
        ""
      } else if before.ends_with('\n') {
        "\n"
      } else {
        "\n\n"
      };
      format!("{before}{separator}{rendered}")
    } else {
      format!("{before}{rendered}{after}")
    };
    write(&path, &text)?;
  }

  Ok(Some(changes))
}

/// The config text before awshop's block, the block itself and the text after it.
/// Without a block, all text is before it.
fn split<'a>(text: &'a str, path: &Path) -> Result<(&'a str, &'a str, &'a str)> {
  let Some(start) = text.find(BEGIN) else {
    return Ok((text, "", ""));
  };

  let end = text[start..]
    .find(END)
    .map(|offset| start + offset + END.len())
    .ok_or_else(|| {
      format!(
        "awshop's block in {} has no '{END}' line. Fix or remove it.",
        path.display()
      )
    })?;

  // The block owns its final newline.
  let end = if text[end..].starts_with('\n') { end + 1 } else { end };
  Ok((&text[..start], &text[start..end], &text[end..]))
}

/// The generated profiles in a block, by name: their session and their text.
fn entries(block: &str) -> BTreeMap<String, (String, String)> {
  block
    .split("\n\n")
    .filter_map(|text| {
      let text = text.trim();
      let value = |prefix: &str| {
        text
          .lines()
          .find_map(|line| line.strip_prefix(prefix))
          .map(|value| value.trim_end_matches(']').to_owned())
      };
      Some((value("[profile ")?, (value("sso_session = ")?, text.to_owned())))
    })
    .collect()
}

/// The block for the grants that `own` profiles don't cover, plus the `kept` profiles
/// of sessions that couldn't be listed. Empty when there are none.
fn render(own: &AwsConfig, grants: &[Grant], mut profiles: BTreeMap<String, String>) -> String {
  let normalize = |url: &str| url.trim_end_matches('/').to_owned();
  let covered = |grant: &Grant| {
    own.profiles.iter().any(|profile| {
      profile.account.as_deref() == Some(grant.account.as_str())
        && profile.role.as_deref() == Some(grant.role.as_str())
        && (profile.session.as_deref() == Some(grant.session.as_str())
          || profile.sso_start_url.as_deref().map(normalize) == Some(normalize(&grant.start_url)))
    })
  };

  let mut roles: BTreeMap<&str, usize> = BTreeMap::new();
  for grant in grants {
    *roles.entry(grant.account.as_str()).or_default() += 1;
  }

  let mut taken: HashSet<String> = own.profiles.iter().map(|profile| profile.name.clone()).collect();
  taken.extend(profiles.keys().cloned());

  for grant in grants.iter().filter(|grant| !covered(grant)) {
    // One role: the account's name. More: the role too, so each gets its own profile.
    let mut name = slug(&grant.account_name).unwrap_or_else(|| grant.account.clone());
    if roles[grant.account.as_str()] > 1 {
      name = format!("{name}-{}", slug(&grant.role).unwrap_or_default());
    }
    // A name in use gets the account ID, then a number, until it is free.
    if taken.contains(&name) {
      name = format!("{name}-{}", grant.account);
    }
    let base = name.clone();
    let mut number = 2;
    while taken.contains(&name) {
      name = format!("{base}-{number}");
      number += 1;
    }
    taken.insert(name.clone());

    let text = format!(
      "# {}\n[profile {name}]\nsso_session = {}\nsso_account_id = {}\nsso_role_name = {}\nregion = {}",
      grant.account_name, grant.session, grant.account, grant.role, grant.region
    );
    profiles.insert(name, text);
  }

  if profiles.is_empty() {
    return String::new();
  }

  let mut block = format!("{BEGIN}\n");
  for text in profiles.values() {
    block.push_str(&format!("\n{text}\n"));
  }
  block.push_str(&format!("\n{END}\n"));

  block
}

/// A profile name from an account or role name: lowercase words joined by `-`.
fn slug(name: &str) -> Option<String> {
  let slug = name
    .to_lowercase()
    .split(|character: char| !character.is_ascii_alphanumeric())
    .filter(|word| !word.is_empty())
    .collect::<Vec<_>>()
    .join("-");
  (!slug.is_empty()).then_some(slug)
}

/// The access token that `aws sso login` cached for `session`, if any.
fn token(session: &str) -> Result<Option<String>> {
  let digest = sha1_smol::Sha1::from(session).digest().to_string();
  let path = awshop_plugin::home()
    .join(".aws/sso/cache")
    .join(format!("{digest}.json"));

  let bytes = match fs::read(&path) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };

  let cached: Value = serde_json::from_slice(&bytes)?;
  Ok(cached["accessToken"].as_str().map(str::to_owned))
}

/// Every account and role that `session` grants. None when the portal refuses the
/// token, for example after it expired.
fn fetch(agent: &Agent, session: &str, start_url: &str, region: &str, token: &str) -> Result<Option<Vec<Grant>>> {
  let mut accounts = Vec::new();
  let mut next = None;

  loop {
    let page = format!("/assignment/accounts?max_result=100{}", next_token(next.as_deref()));
    let Some(response) = get(agent, region, &page, token)? else {
      return Ok(None);
    };

    accounts.extend(response["accountList"].as_array().into_iter().flatten().cloned());
    next = response["nextToken"].as_str().map(str::to_owned);
    if next.is_none() {
      break;
    }
  }

  let mut grants = Vec::new();
  for chunk in accounts.chunks(PARALLEL) {
    let roles = thread::scope(|scope| {
      let lookups: Vec<_> = chunk
        .iter()
        .map(|account| {
          let account = account["accountId"].as_str().unwrap_or_default();
          // Errors cross the thread as text; boxed errors can't.
          scope.spawn(move || roles(agent, region, token, account).map_err(|error| error.to_string()))
        })
        .collect();

      lookups
        .into_iter()
        .map(|lookup| {
          lookup
            .join()
            .unwrap_or_else(|_| Err("A role lookup failed.".to_owned()))
        })
        .collect::<Vec<_>>()
    });

    for (account, roles) in chunk.iter().zip(roles) {
      let Some(roles) = roles? else {
        return Ok(None);
      };
      for role in roles {
        grants.push(Grant {
          session: session.to_owned(),
          start_url: start_url.to_owned(),
          region: region.to_owned(),
          account: account["accountId"].as_str().unwrap_or_default().to_owned(),
          account_name: account["accountName"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .filter(|character| !character.is_control())
            .collect(),
          role,
        });
      }
    }
  }

  // Portal values end up in the config file, so only what AWS allows in them gets there.
  grants.retain(|grant| {
    !grant.account.is_empty()
      && grant.account.bytes().all(|byte| byte.is_ascii_digit())
      && !grant.role.is_empty()
      && grant
        .role
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"+=,.@_-".contains(&byte))
  });
  Ok(Some(grants))
}

/// The role names an account grants, following pages.
fn roles(agent: &Agent, region: &str, token: &str, account: &str) -> Result<Option<Vec<String>>> {
  let mut roles = Vec::new();
  let mut next = None;

  loop {
    let page = format!(
      "/assignment/roles?account_id={}&max_result=100{}",
      encode(account),
      next_token(next.as_deref())
    );
    let Some(response) = get(agent, region, &page, token)? else {
      return Ok(None);
    };

    roles.extend(
      response["roleList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|role| role["roleName"].as_str().map(str::to_owned)),
    );
    next = response["nextToken"].as_str().map(str::to_owned);
    if next.is_none() {
      return Ok(Some(roles));
    }
  }
}

/// The query parameter for the next page, if there is one.
fn next_token(token: Option<&str>) -> String {
  token
    .map(|token| format!("&next_token={}", encode(token)))
    .unwrap_or_default()
}

/// Percent-encode a query value.
pub fn encode(value: &str) -> String {
  utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// GET a portal path as JSON. None when the portal refuses the token, for example
/// after the login expired. `AWSHOP_SSO_PORTAL` replaces the portal's address, for tests.
fn get(agent: &Agent, region: &str, path: &str, token: &str) -> Result<Option<Value>> {
  let portal = env::var("AWSHOP_SSO_PORTAL").unwrap_or_else(|_| format!("https://portal.sso.{region}.amazonaws.com"));

  let url = format!("{portal}{path}");

  let mut retries = 0;
  loop {
    match agent.get(&url).header("x-amz-sso_bearer_token", token).call() {
      Ok(mut response) => return Ok(Some(serde_json::from_str(&response.body_mut().read_to_string()?)?)),
      Err(ureq::Error::StatusCode(401 | 403)) => return Ok(None),
      // Throttling and passing server errors get two more tries, after a growing pause.
      Err(ureq::Error::StatusCode(429 | 500..=599)) if retries < 2 => {
        retries += 1;
        thread::sleep(Duration::from_millis(500 << retries));
      }
      Err(error) => return Err(format!("SSO portal request failed: {error}").into()),
    }
  }
}

/// Replace the file in one step, keeping its permissions. A symlink, as dotfile
/// managers make, keeps pointing at the updated file.
fn write(path: &Path, text: &str) -> Result<()> {
  let path = fs::canonicalize(path)?;
  let mode = fs::metadata(&path)?.permissions().mode();
  let temporary = path.with_file_name(format!(".awshop-config-{}.tmp", std::process::id()));

  let result = (|| -> Result<()> {
    let mut file = fs::OpenOptions::new()
      .write(true)
      .create_new(true)
      .mode(0o600)
      .open(&temporary)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, &path)?;
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

  fn grant(account: &str, name: &str, role: &str) -> Grant {
    Grant {
      session: "corp".into(),
      start_url: "https://corp.awsapps.com/start/".into(),
      region: "eu-north-1".into(),
      account: account.into(),
      account_name: name.into(),
      role: role.into(),
    }
  }

  #[test]
  fn names_come_from_accounts_and_roles() {
    assert_eq!(slug("Web - Front PRO").as_deref(), Some("web-front-pro"));
    assert_eq!(slug("Data (production)").as_deref(), Some("data-production"));
    assert_eq!(slug("äöå"), None);
  }

  #[test]
  fn own_profiles_win_and_names_do_not_clash() {
    let own = AwsConfig::parse(
      "[sso-session corp]\n[profile mine]\nsso_session = corp\nsso_account_id = 111\nsso_role_name = Admin\n\
       [profile legacy]\nsso_start_url = https://corp.awsapps.com/start\nsso_account_id = 222\nsso_role_name = Admin\n\
       [profile team-dev]\nregion = eu-west-1\n[profile team-dev-333]\nregion = eu-west-1\n",
    );

    let block = render(
      &own,
      &[
        grant("111", "Covered", "Admin"),
        grant("222", "Covered legacy", "Admin"),
        grant("333", "Team Dev", "Admin"),
        grant("444", "Data", "Admin"),
        grant("444", "Data", "ReadOnly"),
        grant("555", "", "Admin"),
      ],
      BTreeMap::new(),
    );

    let names: Vec<_> = AwsConfig::parse(&block)
      .profiles
      .into_iter()
      .map(|profile| profile.name)
      .collect();

    // team-dev and team-dev-333 are taken, so the account gets a number too.
    assert_eq!(names, ["555", "data-admin", "data-readonly", "team-dev-333-2"]);
    assert!(block.starts_with(BEGIN) && block.ends_with(&format!("{END}\n")));
    assert!(render(&own, &[grant("111", "Covered", "Admin")], BTreeMap::new()).is_empty());
  }

  #[test]
  fn profiles_of_unlisted_sessions_are_kept() {
    let own = AwsConfig::parse("[sso-session corp]\n");
    let previous = render(&own, &[grant("111", "Kept", "Admin")], BTreeMap::new());
    let kept: BTreeMap<_, _> = entries(&previous)
      .into_iter()
      .map(|(name, (session, text))| {
        assert_eq!((name.as_str(), session.as_str()), ("kept", "corp"));
        (name, text)
      })
      .collect();

    // A new grant may not take a kept profile's name.
    let block = render(&own, &[grant("222", "Kept", "Admin")], kept);
    let names: Vec<_> = AwsConfig::parse(&block)
      .profiles
      .into_iter()
      .map(|profile| profile.name)
      .collect();

    assert_eq!(names, ["kept", "kept-222"]);
  }

  #[test]
  fn the_block_is_found_and_everything_around_it_kept() {
    let path = Path::new("config");
    let text = format!("[profile a]\n\n{BEGIN}\n[profile b]\n{END}\n# after\n");
    let (before, block, after) = split(&text, path).unwrap();
    assert_eq!((before, after), ("[profile a]\n\n", "# after\n"));
    assert!(block.starts_with(BEGIN) && block.ends_with(&format!("{END}\n")));

    assert_eq!(split("[profile a]\n", path).unwrap(), ("[profile a]\n", "", ""));
    assert!(split(&format!("{BEGIN}\n[profile b]\n"), path).is_err());
  }

  #[test]
  fn query_values_are_encoded() {
    assert_eq!(encode("a+b/c=="), "a%2Bb%2Fc%3D%3D");
    assert_eq!(next_token(None), "");
  }
}
