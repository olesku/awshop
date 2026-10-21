# Configuration

awshop works without a config file. To change its defaults, create:

```
$XDG_CONFIG_HOME/awshop/config.toml     (default: ~/.config/awshop/config.toml)
```

awshop only reads this file, so your comments and formatting stay as you wrote them. Unknown keys and invalid values stop awshop with an error that names the file, so typos don't go unnoticed.

## Example

```toml
# ~/.config/awshop/config.toml

[sso]
# Don't add profiles for SSO accounts by itself; `awshop sync` still does.
sync = false

[eks]
# Write to the kubeconfig file by default instead of starting a session.
scope = "saved"

# Roles offered in the EKS authentication and lookup role menus.
[eks.choices.iam-roles]
platform-reader = "arn:aws:iam::123456789012:role/platform-reader"
platform-admin  = "arn:aws:iam::123456789012:role/platform-admin"
```

## `[sso]`

| Key | Default | Effect |
| --- | --- | --- |
| `sync` | `true` | Keep profiles for every account and role your SSO sessions grant in the AWS config. See [Accounts from SSO](sso.md). |

## Plugin tables

Each plugin reads its own table, named after the plugin command (`[eks]` for `awshop eks`). `awshop plugins` lists the available names.

### `scope`

The default scope for plugins that support shell sessions.

| Value | Meaning |
| --- | --- |
| `"shell"` | A temporary session in the current shell. This is the default. |
| `"saved"` | Write to the saved configuration, as with `--saved`. |

**Change scope** in the setup screen still switches between the two for a single run.

### `choices.<collection>`

Named values offered in a plugin's saved-choice menus, as `name = "value"` pairs. They appear next to the values you name inside awshop.

| Plugin | Collection | Used by |
| --- | --- | --- |
| `eks` | `iam-roles` | **Authentication** and **Lookup role** |

How names are handled:
- **Sharing across machines:** values from the config file are never copied into awshop's state, so you can share the file between machines or keep it in a dotfiles repository.
- **Name clashes:** if the config file and the state use the same name, the config file wins.
- **New names:** a name entered in awshop can't take a name already used for another value.

## Not in this file

- **Prompt segments.** They are environment variables (`AWSHOP_PROMPT_SEGMENTS`, `AWSHOP_PROMPT_EKS`), because the prompt runs on every line and doesn't read files. See [Prompt](prompt.md).
- **The AWS config.** awshop reads `AWS_CONFIG_FILE` and `AWS_SHARED_CREDENTIALS_FILE` (default `~/.aws/`) like the AWS CLI. It writes only the marked block of profiles from SSO at the end of the config, never anything else. See [Accounts from SSO](sso.md).

## State

What awshop remembers by itself is kept separately, in:

```
$XDG_STATE_HOME/awshop/preferences.json (default: ~/.local/state/awshop/preferences.json)
```

- **Contents:** settings that worked, per plugin, profile and resource (for EKS: context name, kubeconfig file and roles), the values you named inside awshop, and the profiles you set most recently, which the selector lists first. It holds no credentials.
- **When it changes:** the file is written only after an action succeeds. It is safe to delete; awshop then starts with defaults.
- **Older versions:** they kept this file in `~/.config/awshop/preferences.json`. It is still read, and moved to the new location the next time awshop saves.
