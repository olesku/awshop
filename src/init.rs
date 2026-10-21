//! The shell integrations, built into the binary and printed by `awshop init`.
use awshop_plugin::Result;
use std::{
  env,
  io::{self, Write},
  path::Path,
};

/// The shell functions, built into the binary.
const ZSH_INTEGRATION: &str = include_str!("../completions/awshop.zsh");
const BASH_INTEGRATION: &str = include_str!("../completions/awshop.bash");
const FISH_INTEGRATION: &str = include_str!("../completions/awshop.fish");

/// Print the shell integration for `eval "$(awshop init zsh)"` in a shell rc file.
pub fn print(args: &[String]) -> Result<()> {
  let script = match args {
    [shell] if shell == "zsh" => ZSH_INTEGRATION,
    [shell] if shell == "bash" => BASH_INTEGRATION,
    [shell] if shell == "fish" => FISH_INTEGRATION,
    _ => return Err("Usage: awshop init zsh|bash|fish".into()),
  };

  io::stdout().write_all(script.as_bytes())?;
  Ok(())
}

/// Setup instruction for the user's login shell, or for zsh and bash.
pub fn hint() -> String {
  let shell = env::var("SHELL").unwrap_or_default();
  match Path::new(&shell).file_name().and_then(|name| name.to_str()) {
    Some(name @ ("zsh" | "bash")) => {
      format!("add eval \"$(awshop init {name})\" to ~/.{name}rc")
    }
    Some("fish") => "add awshop init fish | source to ~/.config/fish/config.fish".to_owned(),
    _ => "add eval \"$(awshop init zsh)\" to ~/.zshrc or eval \"$(awshop init bash)\" to ~/.bashrc".to_owned(),
  }
}
