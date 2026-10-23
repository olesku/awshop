//! Generic plugin host: dependencies, AWS context, navigation and preferences.
use crate::{
  aws::{self, AwsConfig, Login},
  cache,
  config::{Config, Scope},
  preferences::{self, Choices, Preferences},
  selector::{choose_profile, Pick, Picker},
  session::{self, Session},
  shell::Op,
  ui::{self, Choice, Item, Menu},
};
use awshop_plugin::{Account, Context, Field, FieldKind, Plugin, Resource, Result, Setup};
use std::{
  collections::BTreeMap,
  env,
  path::{Path, PathBuf},
  process::{Command, Stdio},
  sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
  },
  thread,
};

/// A fresh resource list, or why it could not load.
type Loaded = std::result::Result<Vec<Resource>, String>;

/// Background workers that fetch slower resource details at once.
const DETAIL_WORKERS: usize = 8;

/// Entry point for `awshop <plugin> ...`. `owner` is the PID of the calling shell when
/// the shell function ran us; the returned ops change that shell.
pub fn run(plugin: &'static dyn Plugin, args: &[String], owner: Option<u32>) -> Result<Vec<Op>> {
  if args.iter().any(|arg| arg == "--help" || arg == "-h") {
    let saved = if plugin.session().is_some() { " [--saved]" } else { "" };
    println!(
      "awshop {} [--profile NAME] [--region REGION] [--no-login]{saved} [query]",
      plugin.name()
    );
    println!("{}", plugin.description());
    if plugin.session().is_some() {
      println!("Defaults to This shell (temporary); --saved, or scope = \"saved\" in config.toml, chooses a persistent configuration.");
      println!("Temporary sessions require the shell integration (awshop init). Run awshop {} leave to restore the previous environment.", plugin.name());
    }
    println!("In the list, Ctrl-P changes profile, Ctrl-R changes region and Ctrl-L reloads.");
    println!("Escape goes back; Ctrl-C exits. Setup is applied only after selecting its action.");
    return Ok(Vec::new());
  }

  if args.first().is_some_and(|arg| arg == "leave") {
    if args.len() > 1 {
      return Err(format!("Usage: awshop {} leave", plugin.name()).into());
    }
    let owner = owner.ok_or("Use the awshop shell function to leave a session in the current shell.")?;
    return match session::find(plugin.name(), owner)? {
      Some(session) => session.leave(),
      None => Ok(vec![Op::Say(format!(
        "No {} session is active in this shell.",
        plugin.name()
      ))]),
    };
  }

  let options = Options::parse(args)?;
  Ok(browse(plugin, options, owner)?.map(|(_, ops)| ops).unwrap_or_default())
}

/// Open `plugin` for `profile` from the profile selector. Returns the profile of a
/// successful action with its ops, or None when the user goes back.
pub fn open(
  plugin: &'static dyn Plugin,
  profile: &str,
  no_login: bool,
  owner: Option<u32>,
) -> Result<Option<(String, Vec<Op>)>> {
  if let Err(error) = executables(plugin) {
    ui::notice(&format!("awshop › {}", plugin.title()), &error.to_string())?;
    return Ok(None);
  }

  let options = Options {
    profile: Some(profile.to_owned()),
    no_login,
    nested: true,
    ..Options::default()
  };
  browse(plugin, options, owner)
}

/// The plugin's declared executables, resolved in PATH, or an error with install advice.
/// Optional ones are left out when missing.
pub fn executables(plugin: &dyn Plugin) -> Result<BTreeMap<String, PathBuf>> {
  let mut executables = BTreeMap::new();
  for dependency in plugin.dependencies() {
    let found = aws::find_executable(dependency.name);
    if found.is_none() && dependency.optional {
      continue;
    }

    let path = found.ok_or_else(|| {
      format!(
        "{} requires '{}' in PATH. {}",
        plugin.title(),
        dependency.name,
        dependency.installation
      )
    })?;
    executables.insert(dependency.name.to_owned(), path);
  }

  Ok(executables)
}

/// How a setup is applied. `owner` is the calling shell's PID when the shell function
/// ran us, `active` its current session of this plugin, and `saved` asks for a
/// persistent configuration instead of a session.
struct RunMode {
  saved: bool,
  owner: Option<u32>,
  active: Option<Session>,
}

/// Parsed plugin command line.
#[derive(Default)]
struct Options {
  profile: Option<String>,
  region: Option<String>,
  no_login: bool,
  saved: bool,
  query: String,
  /// Opened from the profile selector, so the first menus can go back to it.
  nested: bool,
}

impl Options {
  /// Options from the arguments after the plugin name.
  fn parse(args: &[String]) -> Result<Self> {
    let mut options = Self::default();
    let mut args = args.iter();
    let mut query = Vec::new();

    while let Some(arg) = args.next() {
      match arg.as_str() {
        "--profile" => options.profile = Some(value(args.next(), "--profile requires a name")?),
        "--region" => options.region = Some(value(args.next(), "--region requires a region")?),
        "--no-login" => options.no_login = true,
        "--saved" => options.saved = true,
        value if value.starts_with('-') => return Err(format!("Unknown plugin option '{value}'.").into()),
        value => query.push(value.to_owned()),
      }
    }

    options.query = query.join(" ");
    Ok(options)
  }
}

/// The value after an option, which must not look like another option.
fn value(arg: Option<&String>, error: &'static str) -> Result<String> {
  Ok(arg.filter(|value| !value.starts_with('-')).ok_or(error)?.clone())
}

/// Named values offered by saved-choice fields: from config.toml (read-only, wins on
/// name clashes) and remembered ones, where new values are added.
struct NamedValues {
  configured: Choices,
  remembered: Choices,
}

impl NamedValues {
  /// All values in a collection by name.
  fn list(&self, collection: &str) -> Vec<(String, String)> {
    let mut values = self.remembered.get(collection).cloned().unwrap_or_default();
    values.extend(self.configured.get(collection).cloned().unwrap_or_default());
    values.into_iter().collect()
  }

  /// The name of a value, if it has one.
  fn name_of(&self, collection: &str, value: &str) -> Option<String> {
    self
      .list(collection)
      .into_iter()
      .find(|(_, candidate)| candidate == value)
      .map(|(name, _)| name)
  }
}

/// Resolve the context, then loop over discovery and the resource list until an
/// action succeeds or the user leaves. Returns the action's profile and ops.
fn browse(plugin: &'static dyn Plugin, options: Options, owner: Option<u32>) -> Result<Option<(String, Vec<Op>)>> {
  let plugin_config = Config::load()?.plugin(plugin.name());
  let executables = executables(plugin)?;
  let config = AwsConfig::load()?;
  let profile = options.profile.unwrap_or_else(aws::active_profile);
  let configured = config
    .profile(&profile)
    .ok_or_else(|| format!("AWS profile '{profile}' was not found. Run awshop to choose a profile."))?;
  let region = options
    .region
    .or_else(|| env::var("AWS_REGION").ok())
    .or_else(|| env::var("AWS_DEFAULT_REGION").ok())
    .or_else(|| configured.region.clone())
    .unwrap_or_default();
  let mut mode = RunMode {
    saved: options.saved || plugin_config.scope == Some(Scope::Saved),
    owner,
    active: None,
  };

  // The value from before the active session lets plugins choose a persistent
  // destination that is not the session's own file.
  // A nested shell, or a direct run, may have inherited another shell's session.
  let mut original_environment = BTreeMap::new();
  if let Some(spec) = plugin.session() {
    if let Some(owner) = owner {
      mode.active = session::find(plugin.name(), owner)?;
    }
    let inherited = match mode.active {
      Some(_) => None,
      None => session::inherited(spec.environment)?,
    };
    if let Some(session) = mode.active.as_ref().or(inherited.as_ref()) {
      original_environment.insert(spec.environment.to_owned(), session.previous().map(str::to_owned));
    }
  }

  let mut context = Context {
    profile,
    region,
    executables,
    original_environment,
    account: None,
  };

  // From here on the screen stays up until the plugin is done. Through the profile
  // selector it is already held.
  let _screen = ui::hold()?;
  if context.region.is_empty() && !change_region(&mut context, &config)? {
    return Ok(None);
  }

  let preferences = Preferences::load()?;
  let title = format!("awshop › {}", plugin.title());

  // Check the login once per profile, also after Ctrl-P changes it.
  let mut authenticated_profile = String::new();
  let mut query = options.query.clone();
  // What a background fetch got, to show next as it is: the list, or why it failed.
  let mut fetched: Option<Loaded> = None;
  // Wait for AWS instead of showing the last list: after Ctrl-L or an error, or when
  // the background fetch found that the profile needs a login.
  let mut wait = false;
  // The resource to highlight in the next list, by ID.
  let mut highlight: Option<String> = None;
  loop {
    // A remembered list only ever bridges the wait: actions use a fresh one. An empty
    // one would hide that AWS is still loading, so it counts as none.
    let check_login = !options.no_login && authenticated_profile != context.profile;
    let remembered = match (&fetched, plugin_config.cache && !wait) {
      (None, true) => cache::load(plugin.name(), &context.profile, &context.region)
        .filter(|remembered| !remembered.resources.is_empty()),
      _ => None,
    };
    let remembered = remembered.and_then(|mut remembered| {
      plugin.mark_current(&context, &mut remembered.resources).ok()?;
      let background = Background::start(plugin, &config, &context, check_login)?;
      Some((remembered, background))
    });
    wait = false;

    // A fresh list gets its current resource marked here, also one fetched before a
    // setup that may have changed it.
    let (loaded, background, age) = match (fetched.take(), remembered) {
      (Some(loaded), _) => (loaded, None, None),
      (None, Some((remembered, background))) => (Ok(remembered.resources), Some(background), Some(remembered.age)),
      (None, None) => {
        ui::busy(&title, &context_lines(&context))?;
        if check_login {
          let profile = config
            .profile(&context.profile)
            .ok_or("Selected AWS profile was not found.")?;
          context.account = config.ensure_login(profile)?;
          authenticated_profile.clone_from(&context.profile);
        }
        (plugin.discover(&context).map_err(|error| error.to_string()), None, None)
      }
    };
    let loaded = match (loaded, &background) {
      (Ok(mut resources), None) => plugin
        .mark_current(&context, &mut resources)
        .map(|_| resources)
        .map_err(|error| error.to_string()),
      (loaded, _) => loaded,
    };
    let mut resources = match loaded {
      Ok(resources) => resources,
      Err(error) => {
        wait = match retry(&title, &error, &mut context, &config, options.nested)? {
          Some(wait) => wait,
          None => return Ok(None),
        };
        continue;
      }
    };

    // Slower columns arrive in the background for a fresh list, from a few workers
    // sharing the resources. Until a worker has them, an empty cell shows what it had
    // last time; a worker without them shows discovery's. A worker stops once the menu
    // is gone and its channel closed. What they find is kept for the cache.
    let (sender, updates) = mpsc::channel();
    let seen = Arc::new(Mutex::new(vec![None; resources.len()]));
    if background.is_none() {
      let next = Arc::new(AtomicUsize::new(0));
      let shared = Arc::new(resources.clone());
      for _ in 0..DETAIL_WORKERS.min(resources.len()) {
        let (sender, context, shared, next, seen) = (
          sender.clone(),
          context.clone(),
          shared.clone(),
          next.clone(),
          seen.clone(),
        );
        // Without a worker, the list just shows no details.
        let _ = thread::Builder::new().spawn(move || loop {
          let index = next.fetch_add(1, Ordering::SeqCst);
          let Some(resource) = shared.get(index) else {
            break;
          };
          let columns = plugin
            .columns(&context, resource)
            .unwrap_or_else(|| resource.columns.clone());
          if let Some(slot) = seen.lock().ok().as_mut().and_then(|seen| seen.get_mut(index)) {
            *slot = Some(columns.clone());
          }
          if sender.send((index, columns)).is_err() {
            break;
          }
        });
      }

      if plugin_config.cache {
        prefill(plugin, &context, &mut resources);
      }
    }

    // A remembered list says so below the rows, which keep their place when the fresh
    // list replaces it.
    let waiting = age
      .map(|age| format!("Last loaded {} · Waiting for AWS…", cache::ago(age)))
      .unwrap_or_default();
    let items: Vec<_> = resources
      .iter()
      .map(|resource| {
        let marker = if resource.current { " *" } else { "" };
        Item::columns(format!("{}{marker}", resource.label), resource.columns.clone()).describe(&waiting)
      })
      .collect();

    // Like the profile selector: * marks the current one, and the list starts there,
    // unless it is shown again for a resource that was highlighted.
    let list_title = if resources.iter().any(|resource| resource.current) {
      format!("{title} · * marks the current one")
    } else {
      title.clone()
    };
    let matching: Vec<_> = items
      .iter()
      .zip(&resources)
      .filter(|(item, _)| item.matches(&query))
      .map(|(_, resource)| resource)
      .collect();
    let selected = matching
      .iter()
      .position(|resource| highlight.as_ref() == Some(&resource.id))
      .or_else(|| matching.iter().position(|resource| resource.current))
      .unwrap_or(0);
    highlight = None;

    let choice = ui::choose(Menu {
      context: &context_lines(&context),
      headings: plugin.headings(),
      search: Some(&query),
      shortcuts: &[('p', "profile"), ('r', "region"), ('l', "reload")],
      empty: "No matching resources. Ctrl-R: region · Ctrl-P: profile · Ctrl-L: retry",
      root: !options.nested,
      updates: Some(&updates),
      refresh: background.as_ref().map(|background| &background.done),
      selected,
      ..Menu::new(&list_title, &items)
    });

    // A fresh list is saved however the menu was left, Ctrl-C included, with the
    // columns the workers found so far. Cells they haven't answered keep what they had.
    if background.is_none() && plugin_config.cache {
      if let Ok(seen) = seen.lock() {
        for (resource, columns) in resources.iter_mut().zip(seen.iter()) {
          if let Some(columns) = columns {
            resource.columns.clone_from(columns);
          }
        }
      }
      // The cache only saves a wait, so failing to write it changes nothing else.
      let _ = cache::save(plugin.name(), &context.profile, &context.region, &resources);
    }

    let index = match choice? {
      Choice::Back => return Ok(None),
      Choice::Shortcut('p') => {
        change_profile(&mut context, &config)?;
        continue;
      }
      Choice::Shortcut('r') => {
        change_region(&mut context, &config)?;
        continue;
      }
      Choice::Shortcut('l') => {
        wait = true;
        continue;
      }
      // The fresh list arrived: show it with the same search and highlight.
      Choice::Refresh(index, filter) => {
        query = filter;
        highlight = index
          .and_then(|index| resources.get(index))
          .map(|resource| resource.id.clone());
        if let Some(background) = background {
          (fetched, wait) = background.finish(&mut context, &mut authenticated_profile);
        }
        continue;
      }
      Choice::Selected(index) => index,
      _ => continue,
    };

    // A choice from a remembered list waits for the fresh one, which must still have it.
    // The fresh list is kept, so going back from the setup shows it, and saved now,
    // since a setup that succeeds doesn't come back to the list.
    let mut resource = resources.swap_remove(index);
    if let Some(background) = background {
      ui::busy(&title, &context_lines(&context))?;
      (fetched, wait) = background.finish(&mut context, &mut authenticated_profile);
      highlight = Some(resource.id.clone());
      let Some(Ok(fresh)) = fetched.as_mut() else {
        continue;
      };
      if plugin_config.cache {
        let mut saved = fresh.clone();
        prefill(plugin, &context, &mut saved);
        let _ = cache::save(plugin.name(), &context.profile, &context.region, &saved);
      }
      let Some(found) = fresh.iter().find(|fresh| fresh.id == resource.id) else {
        ui::notice(
          &format!("{title} › {}", resource.label),
          &format!("{} is no longer in the list from AWS.", resource.label),
        )?;
        continue;
      };
      resource = found.clone();
    }

    let saved = preferences.settings(plugin.name(), &context.profile, &resource.id);
    let mut setup = match plugin.prepare(&context, &resource, &saved) {
      Ok(setup) => setup,
      Err(error) => {
        ui::notice(&format!("{title} › Could not read setup"), &error.to_string())?;
        continue;
      }
    };

    // Named values entered during this setup are committed only after success.
    let mut named = NamedValues {
      configured: plugin_config.choices.clone(),
      remembered: preferences.choices.get(plugin.name()).cloned().unwrap_or_default(),
    };
    if let Some(ops) = configure(plugin, &context, &resource, &mut setup, &mut named, &mut mode)? {
      return Ok(Some((context.profile.clone(), ops)));
    }
  }
}

/// Say why the list could not load, and offer to retry or change the profile or region.
/// None when the user leaves instead; otherwise whether the next list waits for AWS,
/// which a retry does, while another profile or region may show its remembered list.
fn retry(title: &str, error: &str, context: &mut Context, config: &AwsConfig, nested: bool) -> Result<Option<bool>> {
  ui::notice(&format!("{title} › Could not load resources"), error)?;
  let items = [
    Item::new("Retry", ""),
    Item::new("Change profile", ""),
    Item::new("Change region", ""),
  ];
  Ok(
    match ui::choose(Menu {
      context: &context_lines(context),
      root: !nested,
      ..Menu::new(title, &items)
    })? {
      Choice::Selected(0) => Some(true),
      Choice::Selected(1) => {
        change_profile(context, config)?;
        Some(false)
      }
      Choice::Selected(2) => {
        // Without a new region, the next list is the same one again.
        Some(!change_region(context, config)?)
      }
      _ => None,
    },
  )
}

/// Fill empty cells from the list remembered for the context, by resource ID, until
/// the column workers answer.
fn prefill(plugin: &dyn Plugin, context: &Context, resources: &mut [Resource]) {
  let Some(last) = cache::load(plugin.name(), &context.profile, &context.region) else {
    return;
  };
  for resource in resources {
    let Some(previous) = last.resources.iter().find(|previous| previous.id == resource.id) else {
      continue;
    };
    for (cell, previous) in resource.columns.iter_mut().zip(&previous.columns) {
      if cell.is_empty() {
        cell.clone_from(previous);
      }
    }
  }
}

/// A fresh list loading while a remembered one is shown. It never logs in: that needs
/// the terminal, so a needed login waits for the list as without a cache.
struct Background {
  /// Gets a message when the fetch is done, which refreshes the menu.
  done: mpsc::Receiver<()>,
  handle: thread::JoinHandle<Fetched>,
  check_login: bool,
}

/// How a background fetch ended.
enum Fetched {
  /// The list, and the context's account: the one the login check found, if it ran.
  List(Vec<Resource>, Option<Account>),
  /// The profile needs `aws sso login`.
  Login,
  /// AWS or the plugin failed, with the error.
  Failed(String),
}

impl Background {
  /// Fetch the list for the context, checking the login first when `check_login` is set.
  /// None when no thread could be started.
  fn start(plugin: &'static dyn Plugin, config: &AwsConfig, context: &Context, check_login: bool) -> Option<Self> {
    let (sender, done) = mpsc::channel();
    let (config, mut context) = (config.clone(), context.clone());
    let handle = thread::Builder::new()
      .spawn(move || {
        let login = if check_login {
          config
            .profile(&context.profile)
            .ok_or_else(|| "Selected AWS profile was not found.".into())
            .and_then(|profile| config.check_login(profile))
        } else {
          Ok(Login::Valid(context.account.clone()))
        };
        let fetched = match login {
          Ok(Login::Valid(account)) => {
            context.account = account;
            match plugin.discover(&context) {
              Ok(resources) => Fetched::List(resources, context.account.clone()),
              Err(error) => Fetched::Failed(error.to_string()),
            }
          }
          Ok(Login::Needed) => Fetched::Login,
          Err(error) => Fetched::Failed(error.to_string()),
        };
        let _ = sender.send(());
        fetched
      })
      .ok()?;

    Some(Self {
      done,
      handle,
      check_login,
    })
  }

  /// Wait for the fetch. Returns the list or error to show next, or None and that the
  /// next list must wait for a login. A finished login check counts for the profile.
  fn finish(self, context: &mut Context, authenticated_profile: &mut String) -> (Option<Loaded>, bool) {
    match self.handle.join() {
      Ok(Fetched::List(resources, account)) => {
        context.account = account;
        if self.check_login {
          authenticated_profile.clone_from(&context.profile);
        }
        (Some(Ok(resources)), false)
      }
      Ok(Fetched::Login) => (None, true),
      Ok(Fetched::Failed(error)) => (Some(Err(error)), false),
      Err(_) => (Some(Err("Loading the list stopped unexpectedly.".to_owned())), false),
    }
  }
}

/// The setup screen for one resource. Returns the ops for the shell after a successful
/// action and None when the user goes back.
fn configure(
  plugin: &dyn Plugin,
  context: &Context,
  resource: &Resource,
  setup: &mut Setup,
  named: &mut NamedValues,
  mode: &mut RunMode,
) -> Result<Option<Vec<Op>>> {
  let title = format!("awshop › {} › {}", plugin.title(), resource.label);
  let session_spec = plugin.session();
  let mut temporary = session_spec.is_some() && !mode.saved;
  let mut selected = 0;

  // With nothing to review or change, the screen would only ask for confirmation, so
  // the action runs at once. If it fails, the screen shows as usual.
  let mut immediate = setup.fields.is_empty() && session_spec.is_none() && setup.launch.is_none();

  loop {
    // Some while the scope is "This shell".
    let session = session_spec.as_ref().filter(|_| temporary);
    let action = session.map_or(setup.action.as_str(), |spec| spec.action);
    let mut summary = context_lines(context);
    if let Some(spec) = &session_spec {
      summary.push(format!(
        "{:<28} {}",
        spec.scope_label,
        if temporary {
          "This shell (temporary)"
        } else {
          "Saved config"
        }
      ));
    }

    // A temporary session supplies its own file, so that field is hidden.
    let visible_fields: Vec<_> = setup
      .fields
      .iter()
      .enumerate()
      .filter(|(_, field)| session.is_none_or(|spec| spec.file_field != field.key))
      .map(|(index, _)| index)
      .collect();
    for index in &visible_fields {
      let field = &setup.fields[*index];
      let display = match &field.kind {
        FieldKind::SavedChoice {
          default_label,
          collection,
          ..
        } if field.value.is_empty() => (*default_label).to_owned(),
        FieldKind::SavedChoice { collection, .. } => named
          .name_of(collection, &field.value)
          .map(|name| format!("{name} ({})", field.value))
          .unwrap_or_else(|| field.value.clone()),
        _ => field.value.clone(),
      };
      summary.push(format!("{:<28} {display}", field.label));
    }

    if temporary {
      summary.push(format!(
        "Temporary configuration. Leave with awshop {} leave.",
        plugin.name()
      ));
    } else {
      summary.extend(setup.notes.clone());
    }

    let action_description = if temporary {
      "Create a temporary configuration and use it in this shell. Your saved configuration stays unchanged.".to_owned()
    } else {
      setup.notes.join(" ")
    };
    let mut items = vec![Item::new(action, "").describe(action_description)];
    if let Some(launch) = &setup.launch {
      let label = if temporary {
        &launch.session_action
      } else {
        &launch.action
      };
      items.push(Item::new(label, "").describe(&launch.description));
    }
    let launch_rows = usize::from(setup.launch.is_some());
    if session_spec.is_some() {
      items.push(
        Item::new("Change scope", "")
          .describe("Choose between a temporary session in this shell and updating a saved configuration."),
      );
    }
    items.extend(visible_fields.iter().map(|index| {
      Item::new(format!("Change {}", setup.fields[*index].label.to_lowercase()), "")
        .describe(setup.fields[*index].description)
    }));

    // Rows: the action, the launch action if any, "Change scope" for session plugins,
    // then one per field.
    let choice = if std::mem::take(&mut immediate) {
      Choice::Selected(0)
    } else {
      ui::choose(Menu {
        context: &summary,
        selected,
        ..Menu::new(&title, &items)
      })?
    };

    match choice {
      Choice::Back => return Ok(None),
      Choice::Selected(row) if row <= launch_rows => {
        let open = row == 1;
        let settings = setup.settings();
        let pending = if let Some(spec) = session {
          let Some(owner) = mode.owner else {
            ui::notice(
              "Shell integration required",
              &format!(
                "This shell requires the awshop shell function.
To set it up, {}, then open a new shell and run awshop again.
Choose Saved config for direct invocation.",
                crate::init::hint()
              ),
            )?;
            continue;
          };

          // A replaced session is deleted only after the new one succeeds.
          match session::create(plugin.name(), spec, owner, mode.active.as_ref()) {
            Ok(pending) => Some((pending, spec)),
            Err(error) => {
              ui::notice("Could not start session", &error.to_string())?;
              continue;
            }
          }
        } else {
          None
        };

        // The temporary path goes only into this run, never into preferences.
        let mut execution_settings = settings.clone();
        if let Some((pending, spec)) = &pending {
          execution_settings.insert(
            spec.file_field.to_owned(),
            pending.file().to_string_lossy().into_owned(),
          );
        }

        if let Err(error) = execution_settings
          .iter()
          .try_for_each(|(key, value)| plugin.validate_field(key, value))
        {
          ui::notice("Invalid setting", &error.to_string())?;
          continue;
        }

        ui::busy(&title, &summary)?;
        match plugin.execute(context, resource, &execution_settings) {
          Ok(outcome) => {
            // A program opened afterwards uses the session file where activation puts it.
            if let Some((pending, spec)) = &pending {
              execution_settings.insert(
                spec.file_field.to_owned(),
                pending.active_file().to_string_lossy().into_owned(),
              );
            }

            // A saved setup ends the active session.
            let session = match (pending, mode.active.take()) {
              (Some((pending, _)), replacing) => pending.activate(replacing)?,
              (None, Some(active)) => active.leave()?,
              (None, None) => Vec::new(),
            };

            // Messages are ops too, shown once the screen is left. The host owns a
            // session's file and reports the session itself.
            let mut ops = vec![Op::Say(outcome.message)];
            if !temporary {
              ops.extend(outcome.details.into_iter().map(Op::Say));
            }

            // A setup without fields, such as SSM's, has nothing to remember.
            if !setup.fields.is_empty() {
              if let Err(error) = preferences::remember(
                plugin.name(),
                &context.profile,
                &resource.id,
                settings,
                &named.remembered,
              ) {
                ops.push(Op::Say(format!(
                  "Setup succeeded, but preferences could not be saved: {error}"
                )));
              }
            }

            ops.extend(session);

            // Programs get the terminal last; the setup stands whatever they do.
            let launched = open.then(|| plugin.launch(context, resource, &execution_settings));
            for command in outcome.open.map(Ok).into_iter().chain(launched) {
              ops.extend(run_program(command));
            }
            return Ok(Some(ops));
          }
          Err(error) => ui::notice("Setup failed", &error.to_string())?,
        }
      }
      Choice::Selected(row) if row == 1 + launch_rows && session_spec.is_some() => {
        let choices = [
          Item::new("This shell", "Temporary configuration").describe(
            "Use a private temporary file in this shell. Leave the session to restore your previous configuration.",
          ),
          Item::new("Saved config", "Write to the configured file")
            .describe("Update the saved configuration. This change remains after the shell closes."),
        ];
        let scope_label = session_spec.as_ref().map(|spec| spec.scope_label.to_owned());
        if let Choice::Selected(index) = ui::choose(Menu {
          context: scope_label.as_slice(),
          selected: usize::from(!temporary),
          ..Menu::new(&title, &choices)
        })? {
          temporary = index == 0;
          selected = 0;
        }
      }
      Choice::Selected(index) => {
        selected = index;
        let field_index = visible_fields[index - 1 - launch_rows - usize::from(session_spec.is_some())];
        edit_field(plugin, &title, &mut setup.fields[field_index], named)?;
      }
      _ => {}
    }
  }
}

/// Leave the screen, give a program the terminal and wait for it. Returns a message
/// when it could not be started or failed.
fn run_program(command: Result<Command>) -> Option<Op> {
  ui::release();
  let mut command = match command {
    Ok(command) => command,
    Err(error) => return Some(Op::Say(error.to_string())),
  };
  let name = Path::new(command.get_program())
    .file_name()
    .unwrap_or_default()
    .to_string_lossy()
    .into_owned();

  let mut child = match command.stdin(Stdio::inherit()).spawn() {
    Ok(child) => child,
    Err(error) => return Some(Op::Say(format!("Could not start {name}: {error}"))),
  };

  // The program started with the default signal actions; awshop now ignores Ctrl-C,
  // so pressing it inside the program doesn't end awshop underneath it.
  let interrupts = ui::Interrupts::ignore();
  let status = child.wait();
  if let Err(error) = interrupts {
    return Some(Op::Say(format!("Could not ignore Ctrl-C while {name} runs: {error}")));
  }

  match status {
    Ok(status) if status.success() => None,
    Ok(status) => Some(Op::Say(format!("{name} exited with {status}."))),
    Err(error) => Some(Op::Say(format!("Waiting for {name} failed: {error}"))),
  }
}

/// Edit one field until it validates or the user goes back. New named values are
/// added to `named`, which the caller saves only after a successful action.
fn edit_field(plugin: &dyn Plugin, title: &str, field: &mut Field, named: &mut NamedValues) -> Result<()> {
  let mut candidate = field.value.clone();

  loop {
    let value = match &field.kind {
      FieldKind::SavedChoice {
        collection,
        default_label,
        prompt,
      } => {
        let saved = named.list(collection);
        let mut items = vec![Item::new(*default_label, "")];
        items.extend(saved.iter().map(|(name, value)| Item::new(name, value)));
        items.push(
          Item::new("Enter a new value…", *prompt)
            .describe("Enter a value and optionally give it a name to reuse later."),
        );
        let selected = saved
          .iter()
          .position(|(_, value)| *value == field.value)
          .map(|index| index + 1)
          .unwrap_or(0);

        // Rows: the default, the saved values, then "Enter a new value…".
        match ui::choose(Menu {
          context: &[field.label.to_owned()],
          selected,
          ..Menu::new(title, &items)
        })? {
          Choice::Selected(0) => Some(String::new()),
          Choice::Selected(index) if index <= saved.len() => Some(saved[index - 1].1.clone()),
          Choice::Selected(_) => {
            if let Some(value) = ui::input(title, prompt, &candidate)? {
              if let Err(error) = plugin.validate_field(field.key, &value) {
                candidate = value;
                ui::notice("Invalid value", &error.to_string())?;
                continue;
              }

              if !value.is_empty() {
                let default_name = value.rsplit('/').next().unwrap_or(&value);
                if let Some(name) = ui::input(
                  title,
                  "Save this value as (leave empty to use without naming)",
                  default_name,
                )? {
                  if !name.is_empty() {
                    if saved
                      .iter()
                      .any(|(existing, saved)| *existing == name && *saved != value)
                    {
                      ui::notice("Name already used", "Choose a different name for this value.")?;
                      candidate = value;
                      continue;
                    }
                    named
                      .remembered
                      .entry((*collection).into())
                      .or_default()
                      .insert(name, value.clone());
                  }
                }
              }

              Some(value)
            } else {
              None
            }
          }
          _ => None,
        }
      }
      _ => ui::input(title, field.label, &candidate)?,
    };

    let Some(mut value) = value else {
      return Ok(());
    };

    if matches!(field.kind, FieldKind::FilePath) && !value.is_empty() {
      let path = match value.strip_prefix("~/") {
        Some(suffix) => awshop_plugin::home().join(suffix),
        None => PathBuf::from(&value),
      };
      value = env::current_dir()?.join(path).to_string_lossy().into_owned();
    }

    match plugin.validate_field(field.key, &value) {
      Ok(()) => {
        field.value = value;
        return Ok(());
      }
      Err(error) => {
        candidate = value;
        ui::notice("Invalid value", &error.to_string())?;
      }
    }
  }
}

/// Profile and region lines shown above plugin menus.
fn context_lines(context: &Context) -> Vec<String> {
  vec![
    format!("AWS profile  {}", context.profile),
    format!("Region       {}", context.region),
  ]
}

/// Switch the profile for this run only, taking its region when it has one.
fn change_profile(context: &mut Context, config: &AwsConfig) -> Result<()> {
  let picker = Picker {
    title: "awshop › AWS profile",
    description: "Use this AWS profile for this operation. The shell profile stays unchanged.",
    current: &context.profile,
    selected: &context.profile,
    query: "",
    selector: false,
    opens: false,
    refresh: None,
    empty: "No profiles match this filter.",
  };

  if let Pick::Selected(profile) = choose_profile(config, picker)? {
    context.profile.clone_from(&profile.name);
    if let Some(region) = &profile.region {
      context.region.clone_from(region);
    }
  }
  Ok(())
}

/// Pick a region from the configured profiles or enter one. Returns false if cancelled.
fn change_region(context: &mut Context, config: &AwsConfig) -> Result<bool> {
  let mut regions: Vec<_> = config
    .profiles
    .iter()
    .filter_map(|profile| profile.region.clone())
    .collect();
  if !context.region.is_empty() {
    regions.push(context.region.clone());
  }
  regions.sort();
  regions.dedup();

  let mut items: Vec<_> = regions.iter().map(|region| Item::new(region, "")).collect();
  items.push(
    Item::new("Enter region…", "").describe("Enter an AWS region that is not listed in your profile configuration."),
  );
  let selected = regions.iter().position(|region| *region == context.region).unwrap_or(0);

  match ui::choose(Menu {
    selected,
    ..Menu::new("awshop › AWS region", &items)
  })? {
    Choice::Selected(index) if index < regions.len() => {
      context.region.clone_from(&regions[index]);
      Ok(true)
    }
    Choice::Selected(_) => loop {
      let Some(value) = ui::input(
        "awshop › AWS region",
        "Region (for example eu-north-1)",
        &context.region,
      )?
      else {
        return Ok(false);
      };

      if !value.is_empty()
        && value
          .bytes()
          .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
      {
        context.region = value;
        return Ok(true);
      }
      ui::notice("Invalid region", "Enter an AWS region such as eu-north-1.")?;
    },
    _ => Ok(false),
  }
}
