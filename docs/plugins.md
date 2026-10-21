# Plugin development

Plugins are Rust crates compiled into awshop. They use the `awshop-plugin` contract and are registered in one place. The built-in ones are EKS (`crates/awshop-eks`) and SSM (`crates/awshop-ssm`).

## Responsibilities

| Component | Owns |
| --- | --- |
| `awshop-plugin` | Resource, context, setup field and outcome types; `Plugin` trait; subprocess helpers |
| `awshop-eks` | EKS discovery, kubeconfig inspection, IAM role validation, AWS update command |
| `awshop-ssm` | Systems Manager instance discovery and `ssm start-session` |
| `src/plugin.rs` | Dependency checks, profile/region selection, setup navigation and committing preferences |
| `src/aws.rs` | AWS config parsing, SSO login checks and executable lookup, shared with the profile selector |
| `src/ui.rs` | Search, menus, text entry, drawing, resize handling and keyboard input; also used by the profile selector |
| `src/session.rs` | Session directories, manifests, ownership and cleanup |
| `src/shell.rs` | The code channel to the shell function: ops, rendering and quoting |
| `src/preferences.rs` | Remembered settings in the state file (JSON in `$XDG_STATE_HOME`), atomic writes |
| `src/config.rs` | `config.toml`: `[sso]`, and per-plugin `scope` and named `choices` (see [Configuration](config.md)) |
| `src/plugins/mod.rs` | Built-in plugin registry |
| `src/selector.rs` | Profile selector, recent profiles and each profile's menu of actions |
| `src/sso.rs` | Profiles from the SSO portal, in awshop's block of the AWS config (see [Accounts from SSO](sso.md)) |
| `src/main.rs`, `src/init.rs`, `src/profiles.rs` | Command routing and the code channel; `awshop init`; `awshop add` and `--doctor` |
| `completions/awshop.{zsh,bash,fish}` | Shell functions; embedded in the binary and printed by `awshop init zsh\|bash\|fish` |

The plugin crate does not depend on crossterm, the host binary, or the preference file format. It returns data for the host to render. It does not print to stdout, read terminal input, or set the shell environment.

## Lifecycle

1. The host resolves the plugin's declared executables in `PATH` and checks that they are executable. A missing required one stops the plugin with its installation advice; a missing optional one is left out of `Context::executables`.
2. It resolves the profile and region, and checks SSO when appropriate. `--no-login` skips the SSO preflight.
3. `discover(context)` returns resources. Use a stable ID that includes account and region, such as an ARN. Names are for display; IDs scope saved settings. Set `current` on the one in use right now, such as the cluster kubectl points at: the list marks it with `*` and starts there. Keep it to the calls the list needs. For slower extras, such as a version per resource, implement `detail(context, resource)`: the host shows the list at once, calls it for every resource in the background, and replaces each row's detail as answers arrive.
4. After selection, `prepare(context, resource, saved)` describes the setup: action label, fields, defaults and notes. It must not perform the action or write configuration.
5. The host renders the setup and provides editors according to each field's kind. A setup with no fields, no session scope and no `launch` has nothing to review, so the host runs the action as soon as the resource is selected. A field's `description` appears below the list when its editor is highlighted. `validate_field` runs after edits and before execution.
6. Only selecting the action calls `execute(context, resource, settings)`. The outcome contains a one-line confirmation and optional details, such as the file that was written. Keep both short; progress messages are not printed. Details are omitted for temporary sessions, whose files the host manages.
7. On success, the host saves the original field values (never temporary file paths) under plugin → profile → resource ID, along with any named choices. Escape and Ctrl-C do not commit preferences.

AWS commands should use `Context::aws(service, operation)`, which binds the displayed profile and region explicitly, disables the pager and automatic prompts, and uses JSON output. `Context::command(name)` resolves other declared executables. `json` and `output` check exit status and preserve command errors. Pass arguments individually through `Command`; do not build shell command strings.

An action can itself be an interactive program, such as a shell on an instance for SSM: `execute` returns it in `Outcome::open`, and the host gives it the terminal, waits, and shows `message` afterwards. `account(context)` returns the account ID and ARN partition for resource IDs.

A plugin can also offer to open an interactive program after its action, such as k9s for EKS. Declare the program as a `Dependency` with `optional: true`; the host resolves it only when it is in `PATH`. When it is in `Context::executables`, `prepare` returns `Setup::launch` with the action labels, and `launch` builds the command. The host shows the extra action, runs the setup as usual, then gives the program the terminal and waits for it.

## Setup fields

- `Text`: a single editable string.
- `FilePath`: a string editor with `~/` expansion and resolution of relative paths.
- `SavedChoice`: a default option, named saved values and a new-value prompt. The plugin defines the collection key and display labels. EKS uses this for IAM role ARNs; the host does not know about EKS roles.

Settings and named choices must contain non-secret preferences only. Do not return access tokens or credentials as setup fields. Named collections are scoped to a plugin, and users can predefine values for them in `config.toml` under `[<plugin>.choices.<collection>]`; document your collection names in [Configuration](config.md). EKS shares one collection between its authentication and cluster lookup role fields.

## Optional shell sessions

A plugin can implement `session()` and return a `SessionSpec` containing:

- `file_field`: the setup field into which the host injects a temporary file path.
- `environment`: the environment variable that points to that file.
- `scope_label` and `action`: user-facing text for the scope and session action.

EKS declares `kubeconfig`, `KUBECONFIG`, `Kubeconfig scope` and `Start EKS session`. The plugin still executes the same operation with the supplied settings. The host owns everything else: file lifetime, environment changes and cleanup.

- **Default:** session-capable plugins default to **This shell** on every launch; `--saved`, `scope = "saved"` in config.toml or the scope menu selects persistent setup.
- **Setup:** in temporary mode the host hides the destination editor, creates a private session directory, and injects its file path only into the execution settings. If the action fails or is cancelled, the directory is removed.
- **Success:** the host keeps the directory, deletes a session it replaces, and sends the shell function code that exports the variable.
- **Manifest:** each session directory holds a `session.json` with the plugin, the owning shell's PID and start time, the variable and its value from before the first session.
- **Leaving:** `<plugin> leave` restores that value or unsets the variable, unless the user changed it by hand, and deletes the directory. Shell exit removes the shell's sessions, and every run through the shell function removes sessions of shells that are gone.

`Context::original_env(name)` provides the environment from before the current session, taken from the active session's manifest. Use it when choosing a persistent destination; otherwise a plugin might accidentally save into its own temporary file. Discovery may still inspect the current environment to show the active session.

Direct binary invocation can browse and use Saved config. Starting a temporary session requires the shell integration; [Adding a shell](other-shells.md) describes the protocol. Plugins without a `SessionSpec` retain their existing setup behavior.

## Add another plugin

1. Add a workspace crate depending on `awshop-plugin`.
2. Implement `Plugin`, including its dependency descriptions and field validation.
3. Add the crate as a dependency of the main package and register its instance in `src/plugins/mod.rs`.
4. Add tests for discovery parsing and action arguments, plus an interactive smoke test for the new flow.

The host automatically routes the new command, provides generic `--help`, includes it in `awshop plugins`, and lists it in each profile's menu in the selector. The shell functions pass every command to the binary, so they need no changes.

This contract is internal and can evolve while we develop more plugins. There is no external plugin process protocol or dynamic Rust library ABI in this version.

## Checks

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build
python3 tests/eks_pty.py
python3 tests/session_pty.py
python3 tests/sso_pty.py
```

The Python checks use the standard library and Unix pseudoterminals. They run fake `aws`, `kubectl` and `k9s` executables and a fake SSO portal against temporary config files. They do not call AWS or modify your real kubeconfig. The shell session checks run for zsh, Bash and fish, and each shell is skipped if it is absent from `/usr/bin`.
