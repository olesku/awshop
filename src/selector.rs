//! The profile selector: pick a profile and set it in the shell, or open a plugin
//! for it with the right arrow.
use crate::{
  aws::{self, AwsConfig, Profile},
  config::Config,
  console, init, plugin, plugins,
  preferences::{self, Preferences},
  shell::{self, Op, Shell},
  sso, ui,
};
use awshop_plugin::Result;
use std::{
  io::{self, IsTerminal},
  sync::mpsc::{self, Receiver},
  thread,
};

/// AWShop in the ANSI Shadow style, letters four rows tall plus a row of shadow, shown
/// above the profile search.
const LOGO: &[&str] = &[
  " ████╗ ██╗  ██╗ █████╗██╗ ██╗ ████╗ █████╗",
  "██╔═██╗██║█╗██║████╔═╝██████║██╔═██╗██╔═██╗",
  "██████║███████║╚═████╗██╔═██║██║ ██║█████╔╝",
  "██╔═██║╚██╔██╔╝█████╔╝██║ ██║╚████╔╝██╔══╝",
  "╚═╝ ╚═╝ ╚═╝╚═╝ ╚════╝ ╚═╝ ╚═╝ ╚═══╝ ╚═╝",
];

/// The selector for `awshop [--no-login] [--export] [query]`. Returns the ops that set
/// the profile, and those of a plugin opened from it.
pub fn run(mut config: AwsConfig, args: &[String], owner: Option<u32>) -> Result<Vec<Op>> {
  let query = args
    .iter()
    .filter(|arg| !arg.starts_with('-'))
    .cloned()
    .collect::<Vec<_>>()
    .join(" ");
  let no_login = args.iter().any(|arg| arg == "--no-login");
  let active = aws::active_profile();

  // With the shell function, Enter opens the profile's menu, which starts at using it
  // here. Without it, Enter uses the profile at once, so scripts get its name. Going
  // back from the menu returns to this list as it was left.
  let export = args.iter().any(|arg| arg == "--export");
  let menu_first = owner.is_some() && !export;
  let (mut selected, mut query) = (active.clone(), query);

  // Run in a terminal without the shell function, say so before Enter skips the menu.
  let description = if owner.is_none() && !export && io::stdout().is_terminal() {
    format!(
      "No shell integration, so Enter only prints the profile. For EKS, SSM and the console, {}.",
      init::hint()
    )
  } else {
    String::new()
  };

  // Accounts the SSO portal added or removed show up while the list is open. The
  // list is rebuilt when the sync is done, also without changes, so an empty list
  // stops saying it is syncing.
  let (synced, refresh) = mpsc::channel();
  let mut syncing = Config::load()?.sso.sync;
  if syncing {
    thread::spawn(move || {
      let _ = sso::sync();
      let _ = synced.send(());
    });
  }

  let screen = ui::hold()?;
  loop {
    let sessions: Vec<_> = config.sessions.keys().map(String::as_str).collect();
    let empty = if !config.profiles.is_empty() {
      "No profiles match this filter.".to_owned()
    } else if syncing && !sessions.is_empty() {
      "Syncing accounts from SSO…".to_owned()
    } else if let [session, ..] = sessions[..] {
      format!("No profiles yet. Log in with aws sso login --sso-session {session}, then run awshop sync.")
    } else {
      "No AWS profiles. Add one with awshop add.".to_owned()
    };
    let picker = Picker {
      title: "AWS profile selector · * marks the active profile",
      description: &description,
      current: &active,
      selected: &selected,
      query: &query,
      selector: true,
      opens: menu_first,
      refresh: Some(&refresh),
      empty: &empty,
    };
    let reload = match choose_profile(&config, picker)? {
      Pick::Back => return Ok(Vec::new()),
      Pick::Refresh(highlighted, filter) => {
        selected = highlighted.unwrap_or(selected);
        query = filter;
        syncing = false;
        true
      }
      Pick::Selected(profile) => {
        // The SSO login dialog and the result belong in the shell's scrollback.
        drop(screen);
        if !no_login {
          config.ensure_login(profile)?;
        }
        return Ok(use_profile(&profile.name, args, owner));
      }
      Pick::Open(profile, filter) => {
        (selected, query) = (profile.name.clone(), filter);
        match profile_menu(&profile.name, no_login, owner)? {
          Next::Back => false,
          Next::Use => {
            drop(screen);
            if !no_login {
              config.ensure_login(profile)?;
            }
            return Ok(use_profile(&profile.name, args, owner));
          }
          Next::Console => {
            drop(screen);
            if !no_login {
              config.ensure_login(profile)?;
            }
            return Ok(vec![Op::Say(console::open(profile)?)]);
          }

          Next::Applied(name, mut ops) => {
            drop(screen);
            let mut all = use_profile(&name, args, owner);
            all.append(&mut ops);
            return Ok(all);
          }
        }
      }
    };

    if reload {
      config = AwsConfig::load()?;
    }
  }
}

/// Make `name` the shell's profile: ops for the shell function, or printed output
/// without it. `--export` always prints.
fn use_profile(name: &str, args: &[String], owner: Option<u32>) -> Vec<Op> {
  let mut ops = Vec::new();
  if let Err(error) = preferences::remember_profile(name) {
    ops.push(Op::Say(format!("Could not remember the profile as recent: {error}")));
  }

  let export = args.iter().any(|arg| arg == "--export");
  let set = Op::Set("AWS_PROFILE".to_owned(), name.to_owned());
  if owner.is_some() && !export {
    ops.extend([set, Op::Say(format!("Set environment to {name}"))]);
    return ops;
  }

  // Without the shell function, captured stdout gets only the profile name, and a
  // terminal gets an export command, since this process cannot change its shell.
  let interactive = io::stdout().is_terminal();
  if export || interactive {
    println!("{}", shell::render(Shell::Posix, &[set]));
  } else {
    println!("{name}");
  }

  if interactive && !export {
    eprintln!(
      "Tip: {} to switch automatically and get the profile menu with EKS, SSM and the console.",
      init::hint()
    );
    eprintln!(
      "Setup, Oh My Zsh and the prompt: {}#shell-integration",
      env!("CARGO_PKG_REPOSITORY")
    );
  }

  ops
}

/// What the profile's menu led to.
enum Next {
  /// Use the profile in this shell.
  Use,
  /// Open the AWS console as the profile.
  Console,
  /// A plugin's action succeeded: the profile it used and its ops.
  Applied(String, Vec<Op>),
  Back,
}

/// The menu for a profile: use it in this shell, open the AWS console, or open a plugin
/// with it. It always starts at using it, so pressing Enter twice always does the same.
fn profile_menu(profile: &str, no_login: bool, owner: Option<u32>) -> Result<Next> {
  let plugins = plugins::all();
  // Rows: using the profile, the console, then one per plugin.
  let mut items = vec![
    ui::Item::new("Use in this shell", format!("AWS_PROFILE={profile}")),
    ui::Item::new("AWS console", "Sign in to the console in your browser"),
  ];
  items.extend(plugins.iter().map(|plugin| match plugin::executables(*plugin) {
    Ok(_) => ui::Item::new(plugin.title(), plugin.description()),
    Err(error) => ui::Item::new(plugin.title(), "unavailable").describe(error.to_string()),
  }));

  let title = format!("awshop › {profile}");
  let mut selected = 0;
  loop {
    match ui::choose(ui::Menu {
      selected,
      ..ui::Menu::new(&title, &items)
    })? {
      ui::Choice::Selected(0) => return Ok(Next::Use),
      ui::Choice::Selected(1) => return Ok(Next::Console),
      ui::Choice::Selected(index) => {
        // Back from a plugin returns to it in this menu.
        selected = index;
        if let Some((name, ops)) = plugin::open(plugins[index - 2], profile, no_login, owner)? {
          return Ok(Next::Applied(name, ops));
        }
      }
      _ => return Ok(Next::Back),
    }
  }
}

/// How the profile picker was left.
pub enum Pick<'a> {
  Selected(&'a Profile),
  /// Enter on a profile whose menu opens, with the search text to come back to.
  Open(&'a Profile, String),
  /// The profiles changed: the highlighted one and the search text, to show them again.
  Refresh(Option<String>, String),
  Back,
}

/// What the profile picker shows. `*` marks `current`, and the list starts at `selected`.
pub struct Picker<'a> {
  pub title: &'a str,
  pub description: &'a str,
  pub current: &'a str,
  pub selected: &'a str,
  pub query: &'a str,
  /// The selector, where a run starts: it shows the logo, and ← doesn't leave it.
  pub selector: bool,
  /// Enter opens the profile's menu instead of choosing it.
  pub opens: bool,
  /// A message here means the profiles changed and the list must be rebuilt.
  pub refresh: Option<&'a Receiver<()>>,
  /// Shown when no profile matches.
  pub empty: &'a str,
}

/// Profile picker shared by the selector and plugins, with recent profiles first.
pub fn choose_profile<'a>(config: &'a AwsConfig, picker: Picker<'_>) -> Result<Pick<'a>> {
  let Picker {
    title,
    description,
    current,
    selected,
    query,
    selector,
    opens,
    refresh,
    empty,
  } = picker;

  // Recent profiles first, most recent on top; the stable sort keeps the rest in order.
  let recent = Preferences::load()
    .map(|preferences| preferences.recent_profiles)
    .unwrap_or_default();
  let mut profiles: Vec<_> = config.profiles.iter().collect();
  profiles.sort_by_key(|profile| {
    recent
      .iter()
      .position(|name| *name == profile.name)
      .unwrap_or(usize::MAX)
  });

  let items: Vec<_> = profiles
    .iter()
    .map(|profile| {
      let marker = if profile.name == current { " *" } else { "" };
      ui::Item::new(format!("{}{marker}", profile.name), profile.detail()).describe(description)
    })
    .collect();
  let matching: Vec<_> = profiles
    .iter()
    .zip(&items)
    .filter(|(_, item)| item.matches(query))
    .collect();

  let menu = ui::Menu {
    search: Some(query),
    selected: matching
      .iter()
      .position(|(profile, _)| profile.name == selected)
      .unwrap_or(0),
    opens,
    root: selector,
    banner: if selector { LOGO } else { &[] },
    refresh,
    empty,
    ..ui::Menu::new(title, &items)
  };

  Ok(match ui::choose(menu)? {
    ui::Choice::Selected(index) => Pick::Selected(profiles[index]),
    ui::Choice::Open(index, filter) => Pick::Open(profiles[index], filter),
    ui::Choice::Refresh(index, filter) => Pick::Refresh(index.map(|index| profiles[index].name.clone()), filter),
    _ => Pick::Back,
  })
}
