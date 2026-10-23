//! awshop: an interactive AWS CLI profile selector with plugins such as EKS and SSM.
use aws::AwsConfig;
use awshop_plugin::Result;
use shell::{Bridge, Op};
use std::{env, io};

mod aws;
mod cache;
mod config;
mod console;
mod init;
mod plugin;
mod plugins;
mod preferences;
mod profiles;
mod selector;
mod session;
mod shell;
mod sso;
mod ui;

/// Claim the shell function's channel if it ran us, run the command, and send it the ops.
fn run() -> Result<()> {
  let mut args: Vec<String> = env::args().skip(1).collect();

  // The shell function passes `--shell` first. Old versions passed it last.
  let bridge = if args.first().is_some_and(|arg| arg == "--shell") {
    let name = args.get(1).cloned().unwrap_or_default();
    args.drain(..args.len().min(2));
    Some(Bridge::claim(&name)?)
  } else if let Some(index) = args.iter().position(|arg| arg == "--shell") {
    return Err(shell::outdated(args.get(index + 1).map_or("zsh", String::as_str)));
  } else {
    None
  };

  if bridge.is_some() {
    session::sweep();
  }

  let ops = dispatch(&args, bridge.as_ref().map(|bridge| bridge.owner))?;
  match bridge {
    Some(bridge) => bridge.finish(&ops),
    // Without the shell function only messages apply.
    None => {
      for op in ops {
        if let Op::Say(message) = op {
          eprintln!("{message}");
        }
      }
      Ok(())
    }
  }
}

/// Run the command in `args`. With the shell function (`owner` is its PID), changes to
/// the shell are returned as ops; without it, nothing can change the shell.
fn dispatch(args: &[String], owner: Option<u32>) -> Result<Vec<Op>> {
  let command = args.first().map(String::as_str);

  // Plugins have their own help; for every other command, --help only prints help.
  if let Some(plugin) = command.and_then(plugins::find) {
    return plugin::run(plugin, &args[1..], owner);
  }
  if args.iter().any(|arg| arg == "--help" || arg == "-h") {
    print_help();
    return Ok(Vec::new());
  }

  match command {
    Some("__session-cleanup") => {
      let usage = "Usage: awshop __session-cleanup PID";
      let [_, pid] = args else {
        return Err(usage.into());
      };
      session::cleanup_owner(pid.parse().map_err(|_| usage)?)?;
      session::sweep();
      return Ok(Vec::new());
    }
    Some("init") => {
      init::print(&args[1..])?;
      return Ok(Vec::new());
    }
    Some("--version" | "-V") => {
      println!("awshop {}", env!("CARGO_PKG_VERSION"));
      return Ok(Vec::new());
    }
    Some("sync") => {
      let changes = sso::sync()?.ok_or(
        "No valid SSO login to list accounts with. Select an SSO profile in awshop, or run aws sso login --sso-session <name>, then try again.",
      )?;

      for name in &changes.added {
        println!("Added {name}");
      }
      for name in &changes.removed {
        println!("Removed {name}");
      }
      if changes.added.is_empty() && changes.removed.is_empty() {
        println!("Profiles are up to date with SSO.");
      }
      return Ok(Vec::new());
    }
    Some("plugins") => {
      for plugin in plugins::all() {
        println!("{:<12} {}", plugin.name(), plugin.description());
      }
      return Ok(Vec::new());
    }
    _ => {}
  }

  let config = AwsConfig::load()?;
  if command == Some("add") {
    profiles::add(&config, &args[1..])?;
    return Ok(Vec::new());
  }

  if command == Some("console") {
    let name = match &args[1..] {
      [] => aws::active_profile(),
      [name] if !name.starts_with('-') => name.clone(),
      _ => return Err("Usage: awshop console [profile]".into()),
    };
    let profile = config
      .profile(&name)
      .ok_or_else(|| format!("AWS profile '{name}' was not found."))?;
    config.ensure_login(profile)?;
    return Ok(vec![Op::Say(console::open(profile)?)]);
  }

  if let Some(option) = args
    .iter()
    .find(|arg| arg.starts_with('-') && !["--no-login", "--export", "--doctor"].contains(&arg.as_str()))
  {
    return Err(format!("Unknown option '{option}'. See awshop --help.").into());
  }
  if args.iter().any(|arg| arg == "--doctor") {
    profiles::diagnose(&config);
    return Ok(Vec::new());
  }

  selector::run(config, args, owner)
}

/// Usage for the selector and the commands next to it.
fn print_help() {
  println!("awshop [--no-login] [--export] [query]");
  println!("awshop add [profile-name]");
  println!("awshop --doctor");
  println!("awshop sync");
  println!("awshop console [profile]");
  println!("awshop plugins");
  println!("awshop init zsh|bash|fish");
  println!("awshop --version");
  for plugin in plugins::all() {
    println!("awshop {:<10} {}", plugin.name(), plugin.description());
  }
  println!();
  println!("Interactively select an AWS CLI profile. Captured or redirected stdout contains only the profile name.");
  println!("Run in a terminal without the shell function, it prints an export command to copy.");
  println!("Use --export to print that command for eval \"$(awshop --export)\" in bash or zsh.");
  println!("Pass a word or substring to start with a filtered list.");
  println!("With the shell function, Enter on a profile opens its menu: use it in this shell, open the AWS console, or open a plugin such as eks with it. Without it, Enter uses the profile.");
  println!("SSO profiles are checked and logged in when needed. Use --no-login to only switch.");
  println!("Use add to create an SSO profile with the AWS CLI setup wizard.");
  println!("Use --doctor to inspect SSO configuration.");
  println!("Use sync to add profiles for every account and role your SSO sessions grant; awshop also does this in the background.");
  println!("Use init to print the shell integration, for example eval \"$(awshop init zsh)\" in ~/.zshrc.");
  println!("Reads AWS_CONFIG_FILE and AWS_SHARED_CREDENTIALS_FILE when set, otherwise the files in ~/.aws/.");
}

/// Run awshop and report an error, except for Ctrl-C in a menu.
fn main() {
  if let Err(error) = run() {
    // Ctrl-C in a menu ends quietly, like Escape.
    if error
      .downcast_ref::<io::Error>()
      .is_some_and(|error| error.kind() == io::ErrorKind::Interrupted)
    {
      return;
    }

    eprintln!("awshop: {error}");
    std::process::exit(1);
  }
}
