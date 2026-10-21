//! Built-in plugin contract. UI and persistence belong to the awshop host.
//!
//! Plugins discover resources, describe editable setup fields, validate input,
//! and execute the confirmed action. They do not read the terminal or write to
//! stdout. The host checks dependencies and supplies an authenticated context.
use serde_json::Value;
use std::{
  collections::BTreeMap,
  error::Error,
  path::PathBuf,
  process::{Command, Stdio},
};

/// The result type of plugins and the host; errors are shown to the user as text.
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;
/// Field values by key. Stored as preferences, so never put credentials here.
pub type Settings = BTreeMap<String, String>;

/// The user's home directory, or the current directory when it is unknown.
pub fn home() -> PathBuf {
  std::env::home_dir().unwrap_or_default()
}

/// An executable the plugin needs in PATH, with install advice for when it is missing.
/// An optional one is resolved only when present; check `Context::executables` for it.
pub struct Dependency {
  pub name: &'static str,
  pub installation: &'static str,
  pub optional: bool,
}

/// The AWS profile and region the user sees, plus executables resolved by the host.
#[derive(Clone)]
pub struct Context {
  pub profile: String,
  pub region: String,
  pub executables: BTreeMap<String, PathBuf>,
  /// Environment before a shell session, from the host's active session.
  /// A None value records that a variable was originally unset.
  pub original_environment: BTreeMap<String, Option<String>>,
}

impl Context {
  /// A variable's value from before the current awshop session, if one is active.
  /// Use it for persistent destinations so a session never saves into its own file.
  pub fn original_env(&self, name: &str) -> Option<std::ffi::OsString> {
    match self.original_environment.get(name) {
      Some(value) => value.as_ref().map(std::ffi::OsString::from),
      None => std::env::var_os(name),
    }
  }

  /// A command for a declared dependency, with stdin closed.
  pub fn command(&self, name: &str) -> Result<Command> {
    let path = self
      .executables
      .get(name)
      .ok_or_else(|| format!("Required executable '{name}' was not resolved by the host."))?;

    let mut command = Command::new(path);
    command.stdin(Stdio::null());
    Ok(command)
  }

  /// Every AWS call is explicitly bound to the context shown in the UI.
  pub fn aws(&self, service: &str, operation: &str) -> Result<Command> {
    let mut command = self.command("aws")?;
    command
      .args([
        service,
        operation,
        "--profile",
        &self.profile,
        "--region",
        &self.region,
        "--no-cli-pager",
        "--output",
        "json",
      ])
      .env("AWS_PROFILE", &self.profile)
      .env("AWS_REGION", &self.region)
      .env("AWS_DEFAULT_REGION", &self.region)
      .env("AWS_PAGER", "")
      .env("AWS_CLI_AUTO_PROMPT", "off");
    Ok(command)
  }
}

/// Something the user can pick, such as an EKS cluster.
#[derive(Clone)]
pub struct Resource {
  /// Stable, globally scoped identifier, e.g. an ARN (not a display name).
  pub id: String,
  pub label: String,
  pub detail: String,
  /// In use right now, such as the cluster kubectl points at. The list marks it with `*`
  /// and starts there.
  pub current: bool,
}

/// How the host edits a setup field.
pub enum FieldKind {
  Text,
  /// Expands `~/` and makes relative paths absolute.
  FilePath,
  /// The host offers a default, named saved values, and a new value prompt.
  /// Collections are scoped to this plugin; no credentials should be saved.
  SavedChoice {
    collection: &'static str,
    default_label: &'static str,
    prompt: &'static str,
  },
}

/// One editable setting in a setup screen.
pub struct Field {
  pub key: &'static str,
  pub label: &'static str,
  /// Short explanation shown by the host when this setting is highlighted.
  pub description: &'static str,
  pub kind: FieldKind,
  pub value: String,
}

/// What `prepare` returns: the action label, editable fields and notes to show.
pub struct Setup {
  pub action: String,
  /// A second action that also opens an interactive program, built by `Plugin::launch`.
  pub launch: Option<Launch>,
  pub fields: Vec<Field>,
  pub notes: Vec<String>,
}

/// Labels for an action that runs the setup and then opens a program on the terminal,
/// such as k9s.
pub struct Launch {
  /// For Saved config, e.g. "Configure and open k9s".
  pub action: String,
  /// For a temporary session, e.g. "Start EKS session and open k9s".
  pub session_action: String,
  pub description: String,
}

impl Setup {
  /// The fields' current values by key, as `execute` receives and preferences store them.
  pub fn settings(&self) -> Settings {
    self
      .fields
      .iter()
      .map(|field| (field.key.to_owned(), field.value.clone()))
      .collect()
  }
}

/// Result of a successful action: a one-line confirmation and optional short details.
pub struct Outcome {
  pub message: String,
  pub details: Vec<String>,
  /// An interactive program that is the rest of the action, such as a shell on an
  /// instance. The host gives it the terminal and waits; `message` is shown afterwards.
  /// For a program offered next to the action instead, see `Plugin::launch`.
  pub open: Option<Command>,
}

/// Opt into a host-managed temporary configuration and shell environment.
pub struct SessionSpec {
  /// Setup field that receives the temporary file path.
  pub file_field: &'static str,
  /// Environment variable the host points at that file in the calling shell.
  pub environment: &'static str,
  pub scope_label: &'static str,
  pub action: &'static str,
}

/// A built-in awshop command. See docs/plugins.md for the lifecycle. Plugins are shared
/// with background threads, so they must be `Sync`.
pub trait Plugin: Sync {
  /// The command, as in `awshop <name>`.
  fn name(&self) -> &'static str;
  /// The name in titles and menus, such as "EKS".
  fn title(&self) -> &'static str;
  /// One line for `awshop plugins`, help and the profile's menu.
  fn description(&self) -> &'static str;
  /// Executables the plugin runs, checked before anything else.
  fn dependencies(&self) -> &'static [Dependency];
  /// Opt into temporary shell sessions.
  fn session(&self) -> Option<SessionSpec> {
    None
  }
  /// The resources to pick from, such as the clusters in the context's account and
  /// region. Keep it to the calls the list needs; see `detail` for slower extras.
  fn discover(&self, context: &Context) -> Result<Vec<Resource>>;
  /// A fuller `detail` for a resource from a slower lookup, such as a version. The host
  /// shows the list first and fills these in as they arrive. None keeps the detail.
  fn detail(&self, _context: &Context, _resource: &Resource) -> Option<String> {
    None
  }
  /// Describe the setup for a resource. Must not change anything.
  fn prepare(&self, context: &Context, resource: &Resource, saved: &Settings) -> Result<Setup>;
  /// Called after every edit and for all settings before `execute`.
  fn validate_field(&self, field: &str, value: &str) -> Result<()>;
  /// Perform the action with validated settings.
  fn execute(&self, context: &Context, resource: &Resource, settings: &Settings) -> Result<Outcome>;
  /// The program for `Setup::launch`, after `execute` succeeded with the same settings.
  /// The host gives it the terminal and waits for it. For an action that is itself a
  /// program, return it in `Outcome::open` instead.
  fn launch(&self, _context: &Context, _resource: &Resource, _settings: &Settings) -> Result<Command> {
    Err("This plugin has nothing to open.".into())
  }
}

/// Run a command and return stdout, or an error with its stderr.
pub fn output(command: &mut Command, operation: &str) -> Result<Vec<u8>> {
  let result = command.output()?;
  if !result.status.success() {
    return Err(
      format!(
        "{operation} failed ({}).\n{}",
        result.status,
        String::from_utf8_lossy(&result.stderr).trim()
      )
      .into(),
    );
  }
  Ok(result.stdout)
}

/// The AWS account the context's credentials belong to.
pub struct Account {
  pub id: String,
  /// The ARN partition, such as `aws` or `aws-us-gov`.
  pub partition: String,
}

/// Look up the account with `sts get-caller-identity`, for building resource ARNs.
pub fn account(context: &Context) -> Result<Account> {
  let identity = json(&mut context.aws("sts", "get-caller-identity")?, "AWS identity lookup")?;
  let id = identity["Account"]
    .as_str()
    .ok_or("AWS identity did not contain an account ID.")?;
  let partition = identity["Arn"]
    .as_str()
    .and_then(|arn| arn.split(':').nth(1))
    .ok_or("AWS identity did not contain an ARN partition.")?;

  Ok(Account {
    id: id.to_owned(),
    partition: partition.to_owned(),
  })
}

/// Run a command and parse its stdout as JSON.
pub fn json(command: &mut Command, operation: &str) -> Result<Value> {
  let bytes = output(command, operation)?;
  serde_json::from_slice(&bytes).map_err(|error| format!("Invalid JSON from {operation}: {error}").into())
}
