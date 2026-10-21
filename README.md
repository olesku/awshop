<p align="center"><img src="docs/logo.svg" alt="awshop" width="645"></p>

<p align="center"><img src="docs/demo.gif" alt="awshop: switch accounts with SSO login, open an EKS session and an SSM shell" width="800"></p>

# awshop

A terminal UI for hopping between AWS accounts. Pick a profile and it is set in your shell, logged in with SSO when needed. Or go straight on to an EKS cluster or an EC2 instance in that account.

- **Profiles:** awshop uses the profiles in `~/.aws/config` and `~/.aws/credentials`, and adds one for every account your [SSO sessions](#accounts-from-sso) give you. Logging in is left to the AWS CLI.
- **EKS:** pick a cluster and get a kubeconfig for this shell only, optionally opened in k9s.
- **SSM:** pick an EC2 instance and get a shell on it.
- **Console:** open the AWS console in your browser, signed in as the profile.
- **Shell integration:** a function for zsh, Bash or fish changes the shell you run it in. Other shells can use `--export`, or [add a function](docs/other-shells.md).

## Why

Working across many AWS accounts usually means a long, hand-kept `~/.aws/config`, account names you have to remember, a fresh `aws sso login` when credentials run out, and `aws eks update-kubeconfig` runs that leave one shared kubeconfig pointing at whatever cluster you touched last, in every terminal at once. awshop takes care of that:

- **No profiles to maintain.** One `[sso-session]` is enough. awshop asks the SSO portal which accounts you have and keeps the profiles up to date, while profiles you wrote yourself stay as they are.
- **Find, don't remember.** Type part of a name to filter. Your recent profiles come first, and the active one is marked.
- **Logged in when you need it.** awshop checks the SSO login when you pick a profile and runs `aws sso login` only when it has expired.
- **One cluster per terminal.** An EKS session gives this shell its own kubeconfig. Other terminals keep theirs, your saved kubeconfig isn't touched, and the session cleans up after itself. You can't run `kubectl` against prod in the wrong window because another one switched context.
- **From account to cluster or instance in a few keys.** Pick a profile, choose EKS or SSM, pick a cluster or an EC2 instance, and you are in `kubectl`, k9s or a shell on the instance.
- **Safe in your shell.** The shell function only runs code that awshop deliberately sends it, never its regular output, and every value in that code is quoted.

## Contents

- [Why](#why)
- [Quick start](#quick-start)
- [Install](#install)
- [Shell integration](#shell-integration): [zsh](#zsh), [Bash](#bash), [fish](#fish), [other shells](#other-shells)
- [Select a profile](#select-a-profile)
- [Accounts from SSO](#accounts-from-sso)
- [Add a profile](#add-a-profile)
- [AWS console](#aws-console)
- [EKS plugin](#eks-plugin)
- [SSM plugin](#ssm-plugin)
- [Prompt](#prompt)
- [Configuration](#configuration)
- [Inspect SSO setup](#inspect-sso-setup)
- [Development](#development)

## Quick start

1. [Install](#install) awshop and add the [shell integration](#shell-integration), for example `eval "$(awshop init zsh)"` in `~/.zshrc`.
2. If your organization uses AWS IAM Identity Center, this is all the AWS config needs. awshop adds a profile for every account you have:
   ```ini
   # ~/.aws/config
   [sso-session my-sso]
   sso_start_url = https://my-sso.awsapps.com/start
   sso_region = eu-north-1
   sso_registration_scopes = sso:account:access
   ```
   Log in once with `aws sso login --sso-session my-sso`. Profiles you already have keep working as they are.
3. Run `awshop`, pick a profile and press Enter. Its menu opens: Enter again uses it in this shell, or choose EKS or SSM to go on to its clusters or EC2 instances.

## Install

awshop runs on Linux and macOS, on amd64 and arm64. It needs [AWS CLI v2](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-install.html) in `PATH`, plus `kubectl` for the EKS plugin and the Session Manager plugin for SSM.

```sh
curl -fsSL https://raw.githubusercontent.com/olesku/awshop/main/setup.sh | bash
```

- **What it does:** downloads the binary for your system from the [latest release](https://github.com/olesku/awshop/releases/latest), checks its SHA-256 checksum, and installs it as `~/.local/bin/awshop`. It needs no root and doesn't change your rc files; it prints what to add.
- **Options:** `AWSHOP_VERSION=v0.1.0` installs a given release, `AWSHOP_INSTALL_DIR=/usr/local/bin` another location, and `AWSHOP_DOWNLOAD_URL` downloads from a mirror. Pass them to `bash`, as in `curl … | AWSHOP_VERSION=v0.1.0 bash`.
- **Updating:** run the same command again.
- **By hand:** download `awshop-<linux|darwin>-<amd64|arm64>.tar.gz` from the releases page, unpack it, and put the `awshop` from its folder in your `PATH`. `checksums.txt` lists the checksums.
- **From source:** `cargo install --git https://github.com/olesku/awshop`, or `cargo install --path .` in a clone.

Then set up the integration for your shell.

## Shell integration

A program can't change the environment of the shell that started it. The integration is a shell function named `awshop` that runs the binary and evaluates the shell code the binary writes to a separate channel (file descriptor 3), such as `export AWS_PROFILE=dev`. Regular output goes to stdout as usual and is never executed, so `awshop --help` or `awshop init zsh > file` work through the function.

The integration scripts are built into the binary, and `awshop init <shell>` prints the one for your shell. They always match the installed version, so you don't need the source directory.

### zsh

Add to `~/.zshrc`:

```zsh
eval "$(awshop init zsh)"
```

To use the profile in your prompt, see [Prompt](#prompt). zsh needs `setopt PROMPT_SUBST` for that.

With Oh My Zsh, put these three lines after `source "$ZSH/oh-my-zsh.sh"` in `~/.zshrc`, in place of the init line above. They add the awshop context before the theme's existing right prompt, such as the clock in the `clean` theme:

```zsh
eval "$(command awshop init zsh)"
AWSHOP_PROMPT_SEGMENTS="aws region eks"
RPROMPT='$(awshop_prompt) '"$RPROMPT"
```

Oh My Zsh enables `PROMPT_SUBST` already. Themes that manage their own prompt, such as Powerlevel10k, use their own AWS and Kubernetes segments; see [Prompt](docs/prompt.md).

### Bash

Requires Bash 4.2 or newer. macOS ships Bash 3.2, so install a newer version, for example with Homebrew. Add to `~/.bashrc`:

```bash
eval "$(awshop init bash)"
```

With an older Bash, the script prints a warning and defines nothing, and the rest of your `~/.bashrc` still runs.

Sessions are cleaned up from an `EXIT` trap, chained with any trap that already exists when the script loads: the cleanup runs first, then your trap. If you replace the trap later, cleanup happens on the next awshop run instead.

### fish

Add to `~/.config/fish/config.fish`:

```fish
awshop init fish | source
```

fish can't redirect a descriptor into a command substitution, so the function reads the code through a private temporary file instead.

### Without `eval`

To source a file instead, use `completions/awshop.zsh`, `completions/awshop.bash` or `completions/awshop.fish`. The content is identical.

### Other shells

Without the integration, `awshop --export` prints a command for POSIX shells to evaluate:

```sh
eval "$(awshop --export)"
```

If you run the binary directly in a terminal, it prints the same `export` line for you to copy, plus a tip about setup. [Adding a shell](docs/other-shells.md) describes the protocol the functions use and has a function for any POSIX shell.

### Check that it works

```sh
type awshop          # should report a (shell) function
awshop               # select a profile, then:
echo $AWS_PROFILE
```

If `awshop` prints `export AWS_PROFILE=…`, the binary ran instead of the function. Open a new shell, or evaluate the init line again.

## Select a profile

```sh
awshop               # choose from all profiles
awshop prod          # start with the list filtered to "prod"
awshop --no-login    # switch without checking the SSO login, for example offline
```

| Key | In the profile list |
| --- | --- |
| Typing | Filters the list |
| ↑ / ↓ | Moves |
| PgUp / PgDn, Home / End | Moves a page, or to the first or last profile; this works in every list |
| Enter | Opens the profile's menu |
| Escape | Leaves without changes |
| Ctrl-C | Exits, from any menu |

- **The list:** `*` marks the active profile, and the five profiles you set most recently come first. Details, such as the SSO session, role and region, line up in a column.
- **The profile's menu:** **Use in this shell** comes first and is highlighted when the menu opens, so pressing Enter twice sets the profile. Below it are the [AWS console](#aws-console) and the plugins, EKS and SSM. ← or Escape goes back to the profile list as you left it.

  ```
  awshop › platform-dev
  › Use in this shell   AWS_PROFILE=platform-dev
    AWS console         Sign in to the console in your browser
    EKS                 Select an EKS cluster and configure kubectl
    SSM                 Open a shell on an EC2 instance through Systems Manager
  ```
- **Through a plugin:** the profile is set only when the plugin's action succeeds, together with what the plugin sets, such as `KUBECONFIG`.
- **Without the shell function:** in scripts, as in `$(command awshop)`, and with `--export`, Enter uses the profile directly, without the menu.
- **Login:** for SSO profiles, awshop checks the credentials with `aws sts get-caller-identity`. If they don't work, it runs `aws sso login` before switching. Profiles that share an `sso-session` share the login, and role chains (`source_profile`) log in through their SSO source. Other profile types are switched without running AWS commands.
- **Output:** with the integration, the function confirms with `Set environment to <profile>`. When stdout is captured, as in `$(command awshop)`, the binary prints only the profile name, so scripts can use it. `command` skips the function, which sets the profile instead of printing it.

## Accounts from SSO

For every `[sso-session]` in your config, awshop asks the SSO portal which accounts and roles you have and keeps a profile for each, so new accounts appear in the selector by themselves. It writes them to a marked block at the end of `~/.aws/config` and never touches anything outside it. Profiles you wrote yourself win: an account and role they cover gets no generated profile.

```sh
awshop sync          # sync now and list what changed; awshop also syncs in the background
```

It only runs with a valid SSO login and never starts one by itself. See [Accounts from SSO](docs/sso.md) for names, the block format and how to turn it off.

## Add a profile

```sh
awshop add           # asks for a name
awshop add dev
```

This opens the AWS CLI's SSO wizard (`aws configure sso`), where you sign in and choose an account and role. Existing names are refused so you can't overwrite a profile by accident.

When the wizard asks for an SSO session name, use the same one as your other profiles to share their login. The AWS CLI writes the profile itself. With an `sso-session` in place, you rarely need this: [accounts from SSO](#accounts-from-sso) appear by themselves. See the [AWS CLI SSO guide](https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sso.html).

## AWS console

```sh
awshop console              # sign in to the console as the active profile, in your browser
awshop console prod         # as another profile; or choose AWS console in a profile's menu
```

awshop gets the profile's temporary credentials from the AWS CLI, exchanges them for a sign-in token with AWS, and opens the console on the profile's region (`us-east-1` when it sets none). The token never appears on a command line. It needs temporary credentials, from SSO or a role. See [AWS console](docs/console.md).

## EKS plugin

```sh
awshop eks                  # choose a cluster, start a session in this shell
awshop                      # or pick a profile and choose EKS in its menu
awshop eks leave            # end the session and restore KUBECONFIG
awshop eks --saved          # write to your kubeconfig file instead
```

`awshop eks` needs `kubectl`. It lists the clusters for your profile and region with their Kubernetes versions, and `*` marks the one kubectl uses now. It configures access with `aws eks update-kubeconfig`.

- **This shell:** by default you get a temporary kubeconfig for the current shell. Other shells are not affected, and leaving the session or closing the shell removes it.
- **Saved:** `--saved` writes to your kubeconfig file instead.
- **k9s:** when k9s is installed, the setup screen can also open the cluster in it.

Settings that worked are remembered per cluster. See [EKS plugin](docs/eks.md) for scopes, authentication and roles, context names and permissions.

## SSM plugin

```sh
awshop ssm                  # choose an EC2 instance, open a shell on it
```

`awshop ssm` needs the [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html). It lists the EC2 instances that Systems Manager manages in your profile and region, by `Name` tag, and opens `aws ssm start-session` on the one you choose. Exit the shell to return.

See [SSM plugin](docs/ssm.md) for requirements and permissions.

## Prompt

The integration defines `awshop_prompt`, which prints `aws:<profile>` when a profile is set and nothing otherwise. You can add the region and the current EKS context:

```zsh
setopt PROMPT_SUBST                          # zsh only
AWSHOP_PROMPT_SEGMENTS="aws region eks"
PROMPT='%~ $(awshop_prompt) %# '             # Bash: PS1='\w $(awshop_prompt) \$ '
# aws:dev@eu-north-1 eks:dev-1*
```

See [Prompt](docs/prompt.md) for all segments and options.

## Configuration

awshop works without configuration. To set defaults, such as the EKS scope or roles to offer, or to turn off the SSO sync, create `~/.config/awshop/config.toml`. See [Configuration](docs/config.md) for the available options and for where awshop keeps what it remembers.

## Inspect SSO setup

```sh
awshop --doctor
```

This reports:
- **SSO type:** which profiles use shared SSO sessions and which use legacy SSO settings.
- **Unused and undefined sessions:** `[sso-session]` sections that no profile uses, and profiles that point to a session that doesn't exist.
- **Groups:** legacy profiles that could share a session.
- **Role chains.**

It doesn't print account IDs or start URLs, and it doesn't change anything.

To share one login, each profile needs `sso_session` pointing to an `[sso-session]` section. With such a section in place, [awshop sync](#accounts-from-sso) writes these profiles for you; see [Accounts from SSO](docs/sso.md).

`AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` are respected, as in the AWS CLI.

## Development

Try the development build in the current shell:

```zsh
cargo build
export PATH="$PWD/target/debug:$PATH"
eval "$(awshop init zsh)"     # or: eval "$(awshop init bash)", awshop init fish | source
```

Checks:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build
python3 tests/eks_pty.py
python3 tests/session_pty.py   # runs with zsh, Bash and fish
python3 tests/sso_pty.py       # against a fake SSO portal
```

CI runs these on every pull request and push to `main`. To publish a release, see [Releasing](docs/releasing.md).

The Python tests run the real binary in a pseudoterminal. They use fake `aws`, `kubectl` and `k9s` programs, a fake SSO portal and temporary config files, so they never call AWS or touch your own files.

To add a plugin, see [Plugin development](docs/plugins.md).

The demo above is recorded with [VHS](https://github.com/charmbracelet/vhs) from `demo/awshop.tape`, with made-up accounts and fake AWS tools from `demo/`: run `cargo build --release && vhs demo/awshop.tape`.

## License

[MIT](LICENSE)
