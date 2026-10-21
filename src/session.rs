//! Shell sessions: private directories under `$XDG_RUNTIME_DIR/awshop/sessions/`, one
//! per shell and plugin, each holding a manifest and the plugin's configuration file.
//!
//! Only this module touches session paths and manifests. It deletes only the two known
//! file names and then the directory, never recursively and never outside the root.
use crate::shell::{self, Op};
use awshop_plugin::{Result, SessionSpec};
use serde::{Deserialize, Serialize};
use std::{
  env,
  fs::{self, DirBuilder, OpenOptions, Permissions},
  io::{self, Write},
  os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
  path::{Path, PathBuf},
};

/// The file names in a session directory: the manifest and the plugin's file.
const MANIFEST: &str = "session.json";
const CONFIG: &str = "config";

/// Everything the host needs to find, replace, leave and clean up a session.
#[derive(Serialize, Deserialize)]
struct Manifest {
  version: u8,
  plugin: String,
  owner_pid: u32,
  /// The owner's process start time, which tells a reused PID apart.
  owner_start: Option<String>,
  variable: String,
  /// The variable's value before the first session; None means unset.
  previous: Option<String>,
}

/// An active session, read from its manifest.
pub struct Session {
  directory: PathBuf,
  manifest: Manifest,
}

/// A session being set up, in a hidden directory that `find` ignores. Dropping it
/// deletes the directory.
pub struct Pending {
  directory: PathBuf,
  /// Where `activate` moves the directory: the same name without the leading dot.
  active: PathBuf,
  manifest: Manifest,
  /// Set once `activate` has moved the directory, so dropping leaves it.
  activated: bool,
}

/// The session of another shell, such as a parent shell, whose file `variable` points
/// at. A nested shell inherits that value, but its baseline is the session's `previous`.
pub fn inherited(variable: &str) -> Result<Option<Session>> {
  let Some(value) = env::var_os(variable) else {
    return Ok(None);
  };

  let file = Path::new(&value);
  let Some(directory) = file.parent().filter(|_| file.file_name() == Some(CONFIG.as_ref())) else {
    return Ok(None);
  };
  if directory.parent() != Some(root()?.as_path()) {
    return Ok(None);
  }

  Ok(read(directory.to_path_buf())?.filter(|session| session.manifest.variable == variable))
}

/// The active session of `owner` for `plugin`, if any.
pub fn find(plugin: &str, owner: u32) -> Result<Option<Session>> {
  Ok(
    owned(owner)?
      .into_iter()
      .find(|session| session.manifest.plugin == plugin),
  )
}

/// A new session directory. `previous` comes from `replacing`, from an inherited
/// session, or from the environment.
/// Fails if another session of the same owner manages the same variable.
pub fn create(plugin: &str, spec: &SessionSpec, owner: u32, replacing: Option<&Session>) -> Result<Pending> {
  shell::check_name(spec.environment)?;
  if let Some(other) = owned(owner)?
    .iter()
    .find(|session| session.manifest.plugin != plugin && session.manifest.variable == spec.environment)
  {
    return Err(
      format!(
        "{} is already managed by the {} session.",
        spec.environment, other.manifest.plugin
      )
      .into(),
    );
  }

  let manifest = Manifest {
    version: 2,
    plugin: plugin.to_owned(),
    owner_pid: owner,
    owner_start: start_time(owner),
    variable: spec.environment.to_owned(),
    previous: match replacing {
      Some(session) => session.manifest.previous.clone(),
      None => match inherited(spec.environment)? {
        Some(session) => session.manifest.previous,
        None => env::var(spec.environment).ok(),
      },
    },
  };

  // tempfile only picks a fresh name; dropping `Pending` deletes the directory with
  // `remove`, never recursively.
  let directory = tempfile::Builder::new()
    .prefix(&format!(".{plugin}-"))
    .permissions(Permissions::from_mode(0o700))
    .tempdir_in(root()?)?
    .keep();
  let active = directory
    .file_name()
    .and_then(|name| name.to_str()?.strip_prefix('.'))
    .map(|name| directory.with_file_name(name))
    .ok_or("Invalid pending session directory.")?;
  let pending = Pending {
    directory,
    active,
    manifest,
    activated: false,
  };

  OpenOptions::new()
    .write(true)
    .create_new(true)
    .mode(0o600)
    .open(pending.directory.join(MANIFEST))?
    .write_all(&serde_json::to_vec(&pending.manifest)?)?;
  Ok(pending)
}

impl Pending {
  /// The file the plugin writes.
  pub fn file(&self) -> PathBuf {
    self.directory.join(CONFIG)
  }

  /// The same file once the session is active, for programs opened after `activate`.
  pub fn active_file(&self) -> PathBuf {
    self.active.join(CONFIG)
  }

  /// Keep the session after a successful setup, delete the one it replaces, and
  /// return the ops that point the shell at it.
  pub fn activate(mut self, replacing: Option<Session>) -> Result<Vec<Op>> {
    let file = self.file();
    if !fs::symlink_metadata(&file)?.is_file() {
      return Err("Plugin did not create a regular session configuration file.".into());
    }
    fs::set_permissions(&file, Permissions::from_mode(0o600))?;

    let file = self
      .active_file()
      .into_os_string()
      .into_string()
      .map_err(|_| "Session file path is not valid UTF-8.")?;

    // The new session goes live first. If removing the old one then fails, the old
    // directory is left for the exit hook or a sweep, and the shell keeps pointing at it.
    fs::rename(&self.directory, &self.active)?;
    self.activated = true;
    if let Some(session) = replacing {
      remove(&session.directory)?;
    }

    Ok(vec![
      Op::Set(self.manifest.variable.clone(), file),
      Op::Say(format!(
        "Session active in this shell. Leave with: awshop {} leave",
        self.manifest.plugin
      )),
    ])
  }
}

impl Drop for Pending {
  fn drop(&mut self) {
    if !self.activated {
      let _ = remove(&self.directory);
    }
  }
}

impl Session {
  /// Delete the session and restore the variable, unless it was changed by hand.
  pub fn leave(self) -> Result<Vec<Op>> {
    let Manifest {
      plugin,
      variable,
      previous,
      ..
    } = self.manifest;

    let file = self.directory.join(CONFIG);
    let ops = if env::var_os(&variable).is_some_and(|value| value == file.as_os_str()) {
      vec![
        match previous {
          Some(value) => Op::Set(variable, value),
          None => Op::Unset(variable),
        },
        Op::Say(format!("Left {plugin} session.")),
      ]
    } else {
      vec![Op::Say(format!(
        "Keeping {variable}: it was changed after starting the session."
      ))]
    };

    remove(&self.directory)?;
    Ok(ops)
  }

  /// The variable's value before the session, for `Context::original_environment`.
  pub fn previous(&self) -> Option<&str> {
    self.manifest.previous.as_deref()
  }
}

/// Delete all sessions of `owner`, when its shell exits.
pub fn cleanup_owner(owner: u32) -> Result<()> {
  for session in sessions()?.into_iter().flatten() {
    if session.manifest.owner_pid == owner {
      remove(&session.directory)?;
    }
  }

  Ok(())
}

/// Delete sessions whose owner process is gone. Never fails the caller.
pub fn sweep() {
  let Ok(sessions) = sessions() else {
    return;
  };

  for session in sessions.into_iter().flatten() {
    let manifest = &session.manifest;
    let Ok(pid) = libc::pid_t::try_from(manifest.owner_pid) else {
      continue;
    };

    // SAFETY: signal 0 only checks that the process exists.
    let gone = unsafe { libc::kill(pid, 0) } == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);

    // A start time that can't be read now proves nothing.
    let reused = manifest
      .owner_start
      .as_ref()
      .is_some_and(|start| start_time(manifest.owner_pid).is_some_and(|current| current != *start));

    if gone || reused {
      let _ = remove(&session.directory);
    }
  }
}

/// Active sessions of `owner`, without pending ones. A directory with an invalid
/// manifest, such as one from a newer awshop, is left out.
fn owned(owner: u32) -> Result<Vec<Session>> {
  let mut owned = Vec::new();

  for session in sessions()?.into_iter().flatten() {
    let pending = session
      .directory
      .file_name()
      .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
    if session.manifest.owner_pid == owner && !pending {
      owned.push(session);
    }
  }

  Ok(owned)
}

/// Every session directory with a manifest, read and validated, pending ones included.
/// Directories without a manifest can't be identified, so they are left out.
fn sessions() -> Result<Vec<Result<Session>>> {
  let mut sessions = Vec::new();

  // Another shell's exit hook may remove a directory while this one lists them.
  for entry in fs::read_dir(root()?)?.flatten() {
    if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
      sessions.extend(read(entry.path()).transpose());
    }
  }

  Ok(sessions)
}

/// The session in `directory`, or None without a manifest. The only manifest reader.
fn read(directory: PathBuf) -> Result<Option<Session>> {
  let bytes = match fs::read(directory.join(MANIFEST)) {
    Ok(bytes) => bytes,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
    Err(error) => return Err(error.into()),
  };

  serde_json::from_slice::<Manifest>(&bytes)
    .ok()
    .filter(|manifest| manifest.version == 2 && manifest.owner_pid > 0 && shell::check_name(&manifest.variable).is_ok())
    .map(|manifest| {
      Some(Session {
        directory: directory.clone(),
        manifest,
      })
    })
    .ok_or_else(|| format!("Invalid session manifest in {}.", directory.display()).into())
}

/// A path in awshop's private directory, next to the session root, for a file that only
/// this user may read.
pub fn private_file(name: &str) -> Result<PathBuf> {
  Ok(root()?.with_file_name(name))
}

/// The session root, created on first use. Both levels must be private to this user.
fn root() -> Result<PathBuf> {
  // SAFETY: getuid cannot fail.
  let uid = unsafe { libc::getuid() };
  let base = match env::var_os("XDG_RUNTIME_DIR").filter(|value| !value.is_empty()) {
    Some(runtime) => PathBuf::from(runtime).join("awshop"),
    None => env::temp_dir().join(format!("awshop-{uid}")),
  };
  let root = base.join("sessions");

  for directory in [&base, &root] {
    match DirBuilder::new().mode(0o700).create(directory) {
      Err(error) if error.kind() != io::ErrorKind::AlreadyExists => return Err(error.into()),
      _ => {}
    }

    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
      return Err(
        format!(
          "Refusing to use {}: it must be a private directory owned by you.",
          directory.display()
        )
        .into(),
      );
    }
  }

  Ok(root)
}

/// Delete a session's known files and its directory.
fn remove(directory: &Path) -> Result<()> {
  match fs::remove_file(directory.join(CONFIG)) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
    _ => {}
  }

  match fs::remove_file(directory.join(MANIFEST)) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error.into()),
    _ => {}
  }

  fs::remove_dir(directory)?;
  Ok(())
}

/// When process `pid` started, or None if that cannot be read.
#[cfg(target_os = "linux")]
fn start_time(pid: u32) -> Option<String> {
  // Field 22 of stat; the command name before it may contain spaces and ')'.
  let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
  let (_, fields) = stat.rsplit_once(')')?;
  fields.split_whitespace().nth(19).map(str::to_owned)
}

/// When process `pid` started, or None if that cannot be read.
#[cfg(not(target_os = "linux"))]
fn start_time(pid: u32) -> Option<String> {
  let output = std::process::Command::new("ps")
    .args(["-o", "lstart=", "-p", &pid.to_string()])
    .output()
    .ok()?;
  let start = String::from_utf8(output.stdout).ok()?.trim().to_owned();
  (output.status.success() && !start.is_empty()).then_some(start)
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::{Mutex, MutexGuard};

  /// Tests change the process environment, so they run one at a time.
  static ENVIRONMENT: Mutex<()> = Mutex::new(());

  const SPEC: SessionSpec = SessionSpec {
    file_field: "kubeconfig",
    environment: "AWSHOP_TEST_CONFIG",
    scope_label: "",
    action: "",
  };

  /// A private runtime directory for one test, with the test variable unset.
  fn setup() -> (MutexGuard<'static, ()>, tempfile::TempDir) {
    let guard = ENVIRONMENT.lock().unwrap_or_else(|error| error.into_inner());
    let runtime = tempfile::tempdir().unwrap();
    env::set_var("XDG_RUNTIME_DIR", runtime.path());
    env::remove_var(SPEC.environment);
    (guard, runtime)
  }

  fn me() -> u32 {
    std::process::id()
  }

  /// A started session, as after a successful plugin run.
  fn start(plugin: &str, replacing: Option<Session>) -> Vec<Op> {
    let pending = create(plugin, &SPEC, me(), replacing.as_ref()).unwrap();
    fs::write(pending.file(), "config").unwrap();
    pending.activate(replacing).unwrap()
  }

  /// Apply ops to this process's environment, as the shell would.
  fn apply(ops: &[Op]) {
    for op in ops {
      match op {
        Op::Set(name, value) => env::set_var(name, value),
        Op::Unset(name) => env::remove_var(name),
        Op::Say(_) => {}
      }
    }
  }

  fn count() -> usize {
    fs::read_dir(root().unwrap()).unwrap().count()
  }

  #[test]
  fn create_find_and_activate() {
    let _env = setup();
    env::set_var(SPEC.environment, "original");
    assert!(find("eks", me()).unwrap().is_none());

    let ops = start("eks", None);
    let [Op::Set(name, file), Op::Say(message)] = &ops[..] else {
      panic!("unexpected ops");
    };
    assert_eq!(name, SPEC.environment);
    assert!(message.contains("awshop eks leave"));

    let session = find("eks", me()).unwrap().unwrap();
    assert_eq!(session.directory.join(CONFIG).to_str(), Some(file.as_str()));
    assert_eq!(session.previous(), Some("original"));

    assert!(find("other", me()).unwrap().is_none());
    assert!(find("eks", me() + 1).unwrap().is_none());
  }

  #[test]
  fn replacement_keeps_previous_and_deletes_the_old_directory() {
    let _env = setup();
    env::set_var(SPEC.environment, "original");

    apply(&start("eks", None));
    let first = find("eks", me()).unwrap().unwrap().directory;
    apply(&start("eks", find("eks", me()).unwrap()));
    let second = find("eks", me()).unwrap().unwrap();

    assert!(!first.exists() && second.directory.exists());
    assert_eq!(second.previous(), Some("original"));
    assert_eq!(count(), 1);
  }

  #[test]
  fn leave_restores_unsets_or_keeps() {
    let _env = setup();
    env::set_var(SPEC.environment, "original");
    apply(&start("eks", None));
    apply(&find("eks", me()).unwrap().unwrap().leave().unwrap());
    assert_eq!(env::var(SPEC.environment).as_deref(), Ok("original"));

    env::remove_var(SPEC.environment);
    apply(&start("eks", None));
    apply(&find("eks", me()).unwrap().unwrap().leave().unwrap());
    assert!(env::var_os(SPEC.environment).is_none());

    apply(&start("eks", None));
    env::set_var(SPEC.environment, "by hand");
    let ops = find("eks", me()).unwrap().unwrap().leave().unwrap();
    assert!(matches!(&ops[..], [Op::Say(message)] if message.starts_with("Keeping")));
    apply(&ops);
    assert_eq!(env::var(SPEC.environment).as_deref(), Ok("by hand"));
    assert_eq!(count(), 0);
  }

  #[test]
  fn nested_shell_takes_the_baseline_from_the_inherited_session() {
    let _env = setup();
    env::set_var(SPEC.environment, "original");
    apply(&start("eks", None));

    let child = me() + 1;
    assert_eq!(
      inherited(SPEC.environment).unwrap().unwrap().previous(),
      Some("original")
    );

    let pending = create("eks", &SPEC, child, None).unwrap();
    assert_eq!(pending.manifest.previous.as_deref(), Some("original"));

    env::set_var(SPEC.environment, "elsewhere");
    assert!(inherited(SPEC.environment).unwrap().is_none());
  }

  #[test]
  fn plugins_cannot_share_a_variable() {
    let _env = setup();
    start("eks", None);
    let error = create("other", &SPEC, me(), None).err().unwrap();
    assert!(error.to_string().contains("already managed by the eks session"));
    assert_eq!(count(), 1);
  }

  #[test]
  fn active_file_is_where_activation_puts_the_session() {
    let _env = setup();
    let pending = create("eks", &SPEC, me(), None).unwrap();
    let active = pending.active_file();
    fs::write(pending.file(), "config").unwrap();
    let ops = pending.activate(None).unwrap();

    assert!(matches!(&ops[0], Op::Set(_, file) if Path::new(file) == active));
    assert!(active.is_file());
  }

  #[test]
  fn pending_session_is_hidden_until_activated() {
    let _env = setup();
    let pending = create("eks", &SPEC, me(), None).unwrap();
    assert!(find("eks", me()).unwrap().is_none());
    drop(pending);
    assert_eq!(count(), 0);

    // Left behind by a killed awshop: removed when its shell exits.
    std::mem::forget(create("eks", &SPEC, me(), None).unwrap());
    cleanup_owner(me()).unwrap();
    assert_eq!(count(), 0);
  }

  #[test]
  fn sweep_removes_sessions_of_dead_or_replaced_owners() {
    let _env = setup();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    child.wait().unwrap();
    let pending = create("eks", &SPEC, child.id(), None).unwrap();
    let dead = pending.directory.clone();
    std::mem::forget(pending);

    let mut pending = create("eks", &SPEC, me(), None).unwrap();
    let reused = pending.directory.clone();
    pending.manifest.owner_start = Some("0".into());
    fs::write(reused.join(MANIFEST), serde_json::to_vec(&pending.manifest).unwrap()).unwrap();
    std::mem::forget(pending);

    let alive = create("eks", &SPEC, me(), None).unwrap();
    sweep();
    assert!(!dead.exists() && !reused.exists() && alive.directory.exists());
  }

  #[test]
  fn unknown_and_invalid_directories_survive_sweep() {
    let _env = setup();
    let unknown = root().unwrap().join("unknown");
    let invalid = root().unwrap().join("invalid");
    fs::create_dir(&unknown).unwrap();
    fs::create_dir(&invalid).unwrap();
    fs::write(invalid.join(MANIFEST), r#"{"version": 2, "variable": "x"}"#).unwrap();

    sweep();
    assert!(unknown.exists() && invalid.exists());

    // They don't stop other shells from finding their sessions either.
    assert!(find("eks", me()).unwrap().is_none());
  }

  #[test]
  fn shared_or_linked_roots_are_refused() {
    let (_guard, runtime) = setup();
    fs::create_dir(runtime.path().join("awshop")).unwrap();
    fs::set_permissions(runtime.path().join("awshop"), Permissions::from_mode(0o755)).unwrap();
    assert!(root().err().unwrap().to_string().starts_with("Refusing to use"));

    let linked = tempfile::tempdir().unwrap();
    fs::set_permissions(linked.path(), Permissions::from_mode(0o700)).unwrap();
    env::set_var("XDG_RUNTIME_DIR", linked.path());
    std::os::unix::fs::symlink(runtime.path(), linked.path().join("awshop")).unwrap();
    assert!(root().err().unwrap().to_string().starts_with("Refusing to use"));
  }
}
