//! The bridge to the awshop shell function: code it evaluates, sent over fd 3.
//!
//! Only this module writes shell code. Every value in it goes through `quote`.
use awshop_plugin::Result;
use std::{
  env,
  error::Error,
  fs::File,
  io::{self, Write},
  os::fd::{FromRawFd, OwnedFd},
};

/// Syntax of the shell that evaluates our output.
#[derive(Clone, Copy)]
pub enum Shell {
  Posix,
  Fish,
}

/// One change to the calling shell, applied in order.
pub enum Op {
  Set(String, String),
  Unset(String),
  Say(String),
}

/// Code for `ops`, joined with " && " so a failed step stops the rest.
pub fn render(shell: Shell, ops: &[Op]) -> String {
  let code: Vec<_> = ops
    .iter()
    .map(|op| match (op, shell) {
      (Op::Set(name, value), Shell::Posix) => {
        format!("export {name}={}", quote(shell, value))
      }
      (Op::Set(name, value), Shell::Fish) => {
        format!("set -gx {name} {}", quote(shell, value))
      }
      (Op::Unset(name), Shell::Posix) => format!("unset {name}"),
      (Op::Unset(name), Shell::Fish) => format!("set -e {name}"),
      (Op::Say(text), _) => format!("printf '%s\\n' {} >&2", quote(shell, text)),
    })
    .collect();

  code.join(" && ")
}

/// Quote a value as one word. Plain values such as profile names stay readable.
fn quote(shell: Shell, value: &str) -> String {
  if !value.is_empty()
    && value
      .bytes()
      .all(|byte| byte.is_ascii_alphanumeric() || b"_.-@+/:,".contains(&byte))
  {
    return value.to_owned();
  }

  match shell {
    Shell::Posix => format!("'{}'", value.replace('\'', "'\\''")),
    Shell::Fish => format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'")),
  }
}

/// Error unless `name` matches `[A-Z_]+`. Called where names enter the program.
pub fn check_name(name: &str) -> Result<()> {
  if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_uppercase() || byte == b'_') {
    return Err(format!("Invalid environment variable name '{name}'.").into());
  }
  Ok(())
}

/// The shell function that ran us with `--shell`: its syntax, code channel and PID.
pub struct Bridge {
  shell: Shell,
  channel: File,
  pub owner: u32,
}

impl Bridge {
  /// Take over fd 3 and `AWSHOP_SHELL_PID`. Must run before anything opens a file,
  /// since a file could otherwise be given fd 3.
  pub fn claim(name: &str) -> Result<Self> {
    let shell = match name {
      "zsh" | "bash" | "sh" => Shell::Posix,
      "fish" => Shell::Fish,
      _ => return Err(format!("Unknown shell '{name}'. Use zsh, bash, fish or sh.").into()),
    };

    // SAFETY: plain fcntl/close calls on fd 3, which nothing else in this process uses.
    // The duplicate is close-on-exec, so children such as `aws sso login` do not hold
    // the pipe open and keep the shell waiting.
    let channel = unsafe {
      if libc::fcntl(3, libc::F_GETFD) == -1 {
        return Err(outdated(name));
      }

      let fd = libc::fcntl(3, libc::F_DUPFD_CLOEXEC, 10);
      if fd == -1 {
        return Err(io::Error::last_os_error().into());
      }
      let channel = File::from(OwnedFd::from_raw_fd(fd));
      if libc::close(3) == -1 {
        return Err(io::Error::last_os_error().into());
      }

      channel
    };

    let owner = env::var("AWSHOP_SHELL_PID")
      .ok()
      .and_then(|value| value.parse::<u32>().ok())
      .filter(|pid| *pid > 0)
      .ok_or_else(|| outdated(name))?;
    Ok(Self { shell, channel, owner })
  }

  /// Send the code for `ops`, if any, to the shell function.
  pub fn finish(mut self, ops: &[Op]) -> Result<()> {
    if !ops.is_empty() {
      self
        .channel
        .write_all(format!("{}\n", render(self.shell, ops)).as_bytes())?;
    }
    Ok(())
  }
}

/// The error for a shell function that does not speak this protocol.
pub fn outdated(name: &str) -> Box<dyn Error> {
  let init = if name == "fish" {
    "awshop init fish | source".to_owned()
  } else {
    format!("eval \"$(awshop init {name})\"")
  };

  format!("The awshop shell function is out of date or missing. Open a new shell, or run: {init}").into()
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::{path::Path, process::Command};

  const VALUES: &[&str] = &[
    "team-dev",
    "",
    "my profile",
    "it's",
    "say \"hi\"",
    "$HOME $(touch pwned)",
    "`id`",
    "back\\slash\\",
    "two\nlines\n",
    "æøå ☃",
  ];

  #[test]
  fn values_are_quoted_only_when_needed() {
    assert_eq!(quote(Shell::Posix, "team/dev.admin"), "team/dev.admin");
    assert_eq!(quote(Shell::Posix, ""), "''");
    assert_eq!(quote(Shell::Posix, "it's $(x)"), "'it'\\''s $(x)'");
    assert_eq!(quote(Shell::Fish, "a\\b'c$d"), "'a\\\\b\\'c$d'");
  }

  #[test]
  fn ops_render_as_one_chain() {
    let ops = [
      Op::Set("KUBECONFIG".into(), "/tmp/a b".into()),
      Op::Unset("AWS_PROFILE".into()),
      Op::Say("Left it's".into()),
    ];

    assert_eq!(
      render(Shell::Posix, &ops),
      "export KUBECONFIG='/tmp/a b' && unset AWS_PROFILE && printf '%s\\n' 'Left it'\\''s' >&2"
    );
    assert_eq!(
      render(Shell::Fish, &ops),
      "set -gx KUBECONFIG '/tmp/a b' && set -e AWS_PROFILE && printf '%s\\n' 'Left it\\'s' >&2"
    );
    assert_eq!(
      render(Shell::Posix, &[Op::Set("AWS_PROFILE".into(), "dev".into())]),
      "export AWS_PROFILE=dev"
    );
  }

  #[test]
  fn names_are_checked() {
    assert!(check_name("KUBECONFIG").is_ok());
    for name in ["", "kubeconfig", "A-B", "A B", "A;B"] {
      assert!(check_name(name).is_err(), "{name}");
    }
  }

  #[test]
  fn values_round_trip_through_real_shells() {
    let shells: &[(&str, &[&str], Shell, &str)] = &[
      ("sh", &[], Shell::Posix, "\"$AWSHOP_VALUE\""),
      ("bash", &["--norc"], Shell::Posix, "\"$AWSHOP_VALUE\""),
      ("zsh", &["-f"], Shell::Posix, "\"$AWSHOP_VALUE\""),
      ("fish", &["--no-config"], Shell::Fish, "$AWSHOP_VALUE"),
    ];

    // Shells live in /bin on macOS and in /usr/bin on most Linux systems; missing ones
    // are skipped, but at least sh must run.
    let mut ran = 0;
    for (name, flags, shell, reference) in shells {
      let Some(path) = ["/bin", "/usr/bin"]
        .iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.exists())
      else {
        continue;
      };
      let path = path.display().to_string();
      ran += 1;

      for value in VALUES {
        let set = render(*shell, &[Op::Set("AWSHOP_VALUE".into(), (*value).into())]);
        let output = Command::new(&path)
          .args(*flags)
          .arg("-c")
          .arg(format!("{set}; printf %s {reference}"))
          .current_dir(env::temp_dir())
          .output()
          .unwrap();
        assert!(output.status.success(), "{path}: {value:?}");
        assert_eq!(String::from_utf8(output.stdout).unwrap(), *value, "{path}");
      }
    }

    assert!(ran > 0, "no shell found");
  }
}
