//! Sign in to the AWS console as a profile. The AWS CLI exports the profile's temporary
//! credentials, AWS's federation endpoint turns them into a sign-in token, and the
//! browser opens a private page that forwards to the console with it. The token never
//! appears on a command line, where other users of the machine could see it.
use crate::{
  aws::{self, Profile},
  session,
  sso::encode,
};
use awshop_plugin::Result;
use serde_json::{json, Value};
use std::{
  env,
  fs::OpenOptions,
  io::Write,
  os::unix::fs::OpenOptionsExt,
  path::Path,
  process::{Command, Stdio},
  thread,
  time::Duration,
};
use ureq::Agent;

/// Open the console for `profile` in the browser, on the home page of its region.
/// Returns what to tell the user.
pub fn open(profile: &Profile) -> Result<String> {
  let output = Command::new(aws::find_aws_cli()?)
    .args([
      "configure",
      "export-credentials",
      "--profile",
      &profile.name,
      "--format",
      "process",
    ])
    .stdin(Stdio::null())
    .output()?;
  if !output.status.success() {
    return Err(
      format!(
        "Could not get credentials for '{}'.\n{}",
        profile.name,
        String::from_utf8_lossy(&output.stderr).trim()
      )
      .into(),
    );
  }

  let credentials: Value = serde_json::from_slice(&output.stdout)?;
  let field = |key: &str| {
    credentials[key]
      .as_str()
      .ok_or_else(|| format!("The AWS CLI returned no {key} for '{}'.", profile.name))
  };
  // Long-term access keys have no session token, and federation needs one.
  let Some(token) = credentials["SessionToken"].as_str() else {
    return Err(
      format!(
        "The AWS console needs temporary credentials, from SSO or a role. Profile '{}' has long-term access keys.",
        profile.name
      )
      .into(),
    );
  };
  let session = json!({
    "sessionId": field("AccessKeyId")?,
    "sessionKey": field("SecretAccessKey")?,
    "sessionToken": token,
  })
  .to_string();

  let region = profile.region.as_deref().unwrap_or("us-east-1");
  let (signin, console) = endpoints(region);
  // `AWSHOP_SIGNIN` replaces the sign-in endpoint, for tests.
  let signin = env::var("AWSHOP_SIGNIN").unwrap_or(signin);
  let agent: Agent = Agent::config_builder()
    .timeout_global(Some(Duration::from_secs(20)))
    .build()
    .into();
  let response = agent
    .get(&format!(
      "{signin}/federation?Action=getSigninToken&Session={}",
      encode(&session)
    ))
    .call()
    .map_err(|error| format!("AWS console sign-in failed: {error}"))?
    .body_mut()
    .read_to_string()?;
  let response: Value = serde_json::from_str(&response)?;
  let signin_token = response["SigninToken"]
    .as_str()
    .ok_or("AWS console sign-in returned no token.")?;

  let destination = format!("{console}/console/home?region={region}");
  let login = format!(
    "{signin}/federation?Action=login&Issuer=awshop&Destination={}&SigninToken={}",
    encode(&destination),
    encode(signin_token)
  );

  // Only this user can read the page, and the token in it expires within 15 minutes.
  let page = session::private_file("console.html")?;
  OpenOptions::new()
    .write(true)
    .create(true)
    .truncate(true)
    .mode(0o600)
    .open(&page)?
    .write_all(
      format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"refresh\" content=\"0;url={}\"><title>AWS console</title>\n",
        login.replace('&', "&amp;").replace('"', "&quot;")
      )
      .as_bytes(),
    )?;

  // xdg-open and open return at once, but a browser from $BROWSER may run until it is
  // closed. A program that is still running after a moment counts as opened.
  let mut command = browser(env::var("BROWSER").ok().as_deref(), &page);
  let opened = command
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .and_then(|mut child| {
      for _ in 0..20 {
        if let Some(status) = child.try_wait()? {
          return Ok(status.success());
        }
        thread::sleep(Duration::from_millis(100));
      }
      Ok(true)
    });
  if !opened.is_ok_and(|success| success) {
    let program = command.get_program().to_string_lossy();
    let help = if cfg!(target_os = "macos") {
      "Set BROWSER to your browser"
    } else {
      "Set BROWSER to your browser, or check which program opens HTML files with: xdg-mime query default text/html"
    };
    return Err(
      format!(
        "Could not open a browser with {program}. {help}. The sign-in page is {}.",
        page.display()
      )
      .into(),
    );
  }

  Ok(format!("Opened the AWS console for {} in your browser.", profile.name))
}

/// The command that opens `page`: the browser in `$BROWSER` (`configured`) when set,
/// else the system's opener, which uses the default program for HTML files. Like
/// other tools, it takes the first of several browsers separated by `:`, and puts the
/// page where `%s` is, or last. It runs directly, never through a shell.
fn browser(configured: Option<&str>, page: &Path) -> Command {
  let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
  let entry = configured
    .and_then(|value| value.split(':').map(str::trim).find(|entry| !entry.is_empty()))
    .unwrap_or(opener);
  let page = page.to_string_lossy();

  let mut words = entry.split_whitespace();
  let mut command = Command::new(words.next().unwrap_or(opener));
  let mut placed = false;
  for word in words {
    placed |= word.contains("%s");
    command.arg(word.replace("%s", &page));
  }
  if !placed {
    command.arg(page.as_ref());
  }
  command
}

/// The sign-in endpoint and the console's address for a region's partition.
fn endpoints(region: &str) -> (String, String) {
  if region.starts_with("us-gov-") {
    (
      "https://signin.amazonaws-us-gov.com".to_owned(),
      "https://console.amazonaws-us-gov.com".to_owned(),
    )
  } else if region.starts_with("cn-") {
    (
      "https://signin.amazonaws.cn".to_owned(),
      "https://console.amazonaws.cn".to_owned(),
    )
  } else {
    (
      "https://signin.aws.amazon.com".to_owned(),
      format!("https://{region}.console.aws.amazon.com"),
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn browser_comes_from_browser_variable_or_the_system() {
    let page = Path::new("/run/awshop/console.html");
    let words = |configured| {
      let command = browser(configured, page);
      let mut words = vec![command.get_program().to_string_lossy().into_owned()];
      words.extend(command.get_args().map(|arg| arg.to_string_lossy().into_owned()));
      words
    };
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    assert_eq!(words(None), [opener, "/run/awshop/console.html"]);
    assert_eq!(words(Some("")), [opener, "/run/awshop/console.html"]);
    assert_eq!(
      words(Some("firefox --new-window")),
      ["firefox", "--new-window", "/run/awshop/console.html"]
    );
    assert_eq!(
      words(Some(" :chromium:firefox")),
      ["chromium", "/run/awshop/console.html"]
    );
    assert_eq!(
      words(Some("browser --url=%s --x")),
      ["browser", "--url=/run/awshop/console.html", "--x"]
    );
  }

  #[test]
  fn each_partition_has_its_own_endpoints() {
    assert_eq!(endpoints("eu-north-1").1, "https://eu-north-1.console.aws.amazon.com");
    assert_eq!(endpoints("us-gov-west-1").0, "https://signin.amazonaws-us-gov.com");
    assert_eq!(endpoints("cn-north-1").1, "https://console.amazonaws.cn");
  }
}
