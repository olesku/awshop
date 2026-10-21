# Adding a shell

awshop ships integrations for zsh, Bash and fish (`awshop init zsh|bash|fish`). This guide describes the protocol they use, so you can write a function for another shell, and what works without one.

## The contract

A child process can't change its parent shell's environment. The integration is a function named `awshop` that runs the binary and evaluates the code it sends back.

| Part | What the function does |
| --- | --- |
| **Call** | Runs `command awshop --shell <name> ARGS`, with `--shell` first and `AWSHOP_SHELL_PID` set to the shell's own PID. |
| **Code channel** | Opens file descriptor 3 for writing, captures it, and evaluates what arrives there if awshop exits with status 0. Stdout and stderr stay connected as usual. |
| **Exit** | Runs `command awshop __session-cleanup <pid>` when the shell exits, to remove its sessions. |

- **Syntax:** `--shell zsh`, `bash` and `sh` get POSIX code (`export NAME='value'`, `unset NAME`, `printf … >&2`), joined with `&&`. `--shell fish` gets fish code. Use `sh` for any other POSIX shell.
- **Safety:** only the binary writes to fd 3, and every value in that code is quoted. Help text, `awshop init` and other output go to stdout and are never evaluated.
- **Owner:** `AWSHOP_SHELL_PID` identifies the shell that owns a session. A subshell with its own PID doesn't see its parent's session.
- **Missed cleanup:** if the shell is killed, the next awshop run through a shell function, in any shell, removes sessions of shells that are gone.
- **Errors:** without fd 3, or without a valid `AWSHOP_SHELL_PID`, awshop exits with status 1 and says the function is out of date.

## A POSIX function (dash, ksh)

```sh
awshop() {
  { _awshop_code=$(AWSHOP_SHELL_PID=$$ command awshop --shell sh "$@" 3>&1 1>&4 4>&-); } 4>&1 || return
  eval "$_awshop_code"
}
trap 'command awshop __session-cleanup $$ 2>/dev/null' EXIT
```

- **Redirections:** `4>&1` saves the real stdout. Inside the substitution, `3>&1` points fd 3 at the capture, `1>&4` restores stdout and `4>&-` closes the helper.
- **Subshells:** `$$` stays the parent's PID in subshells, so a subshell can leave its parent's session. zsh and Bash use `$sysparams[pid]` and `$BASHPID` to avoid that.
- **No descriptor tricks:** a shell that can't redirect a descriptor into a command substitution can point fd 3 at a private temporary file and read it afterwards, as `completions/awshop.fish` does.

## New syntax

To support a shell whose syntax isn't POSIX, add a variant to `Shell` in `src/shell.rs`. Map its `--shell` name in `Bridge::claim`, and add its form of each `Op` to `render` and its quoting rules to `quote`. The round-trip unit test in `shell.rs` checks quoting against the real shell; add it there.

## Without the integration

The binary also works directly. The menu is drawn on stderr, so stderr and stdin must stay connected to the terminal.

| How awshop runs | Stdout |
| --- | --- |
| stdout is captured or redirected | the selected profile name and a newline |
| `--export`, or stdout is a terminal | `export AWS_PROFILE=<name>` |
| Escape, or Ctrl-C in a menu | nothing (exit status 0) |
| Ctrl-C while awshop waits for AWS | nothing (exit status 130) |
| an error | nothing; the message goes to stderr (exit status 1) |

- **Name only:** there are no colors or extra lines. Treat empty output as "keep the current profile".
- **Quoting:** `--export` quotes the name for POSIX shells. Plain names are printed unquoted. Other names are put in single quotes, with each `'` written as `'\''`.
- **Login:** `aws sso login` runs before anything is printed and writes to stderr. `--no-login` skips it.

```sh
eval "$(awshop --export)"
```

Without the integration, temporary plugin sessions (`awshop eks`, scope **This shell**) and `awshop_prompt` are not available. Use `awshop eks --saved`, which writes your kubeconfig file, and read `AWS_PROFILE` in your prompt.
