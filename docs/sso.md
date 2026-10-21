# Accounts from SSO

If your organization uses AWS IAM Identity Center (SSO), you don't need to write a profile per account. awshop asks the SSO portal which accounts and roles you have and keeps a profile for each one. New accounts show up in the selector by themselves, and accounts you lose access to disappear.

## Getting started

The AWS config needs only the SSO session:

```ini
# ~/.aws/config
[sso-session my-sso]
sso_start_url = https://my-sso.awsapps.com/start
sso_region = eu-north-1
sso_registration_scopes = sso:account:access
```

Then log in once, and sync:

```sh
aws sso login --sso-session my-sso
awshop sync        # lists what it added; running awshop also syncs, in the background
```

The first login opens the browser and asks you to allow `botocore-client-my-sso`: that is the AWS CLI, named after your session. One login covers all the accounts, and the AWS CLI renews it in the background for the rest of the session, typically a working day.

## Where the profiles go

awshop writes them to a marked block at the end of your AWS config (`~/.aws/config`, or `AWS_CONFIG_FILE`):

```ini
# BEGIN awshop: profiles from SSO, kept up to date by awshop. Edits here are replaced.

# Team Data (production)
[profile team-data-production]
sso_session = my-sso
sso_account_id = 222222222222
sso_role_name = Admin
region = eu-north-1

# END awshop
```

- **Names:** the account name in lowercase, with words joined by `-`. An account where you have several roles gets one profile per role, named `<account>-<role>`. An account name without ASCII letters or digits gives the account ID.
- **Region:** the session's `sso_region`.
- **Outside the block, nothing changes:** not your profiles, comments, order or formatting. The file is replaced in one step and keeps its permissions, and a symlink, as dotfile managers make, keeps pointing at the updated file.

## Your own profiles

Profiles you write yourself, outside the block, always win.

- **No duplicates:** if one of your profiles already covers an account and role, in the same `sso-session` or with the same legacy `sso_start_url`, awshop generates none for it. Your names stay, and so does everything awshop remembers for them, such as EKS settings.
- **Name clashes:** a generated name that one of your profiles already uses gets the account ID added, as in `prod-333333333333`, and a number after that if needed.
- **Renaming a generated profile:** move it out of the block and rename it. awshop then sees that your profile covers the account and drops its own.
- **Static only:** profiles without an `[sso-session]`, such as access keys or legacy SSO profiles, work as before and are never changed.

## When it runs

- **In the selector:** every `awshop` run syncs in the background, and the list updates when it is done; the search and the highlighted profile stay. With no profiles yet, the list says `Syncing accounts from SSO…` meanwhile, or how to log in if there is no valid login.
- **On demand:** `awshop sync` syncs now and prints what was added and removed.
- **Only with a valid login.** awshop uses the token that `aws sso login` cached in `~/.aws/sso/cache/`. It never starts a login just to sync.
- **Session by session:** every `[sso-session]` in the config is synced with its own login. A session that can't be listed, because it isn't logged in or lacks `sso_start_url` or `sso_region`, keeps the profiles it already has, while the others are synced.
- **Edits meanwhile:** awshop reads the config again after listing the accounts, so a change made while it listed them, such as `aws configure`, is kept.
- **Speed and safety:** awshop calls the portal over HTTPS itself, eight requests at a time, so a sync takes about a second. Throttling and passing server errors are retried twice. The token stays inside awshop and never appears on a command line, where other users of the machine could see it.

## Turning it off

```toml
# ~/.config/awshop/config.toml
[sso]
sync = false
```

The selector then leaves the AWS config alone; `awshop sync` still works when you run it. To remove the generated profiles, delete the block.
