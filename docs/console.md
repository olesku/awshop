# AWS console

awshop signs you in to the AWS console as a profile, in your browser, on the home page of the profile's `region`, or `us-east-1` when the profile sets none.

```sh
awshop                    # pick a profile, then AWS console in its menu
awshop console            # the active profile
awshop console prod       # a given profile
```

## How it works

1. **Login:** for SSO profiles, awshop checks the login first and runs `aws sso login` when it has expired, as when you select a profile.
2. **Credentials:** `aws configure export-credentials` gives the profile's temporary credentials. This works for SSO profiles, roles and role chains.
3. **Sign-in token:** awshop exchanges them at AWS's federation endpoint (`signin.aws.amazon.com`) for a sign-in token, over HTTPS from awshop itself.
4. **Browser:** awshop writes a small page that forwards to the console with the token, and opens it in your browser.

## Which browser

- **`$BROWSER` first.** When it is set, awshop runs it with the page, for example `BROWSER="firefox --new-window"`. As in other tools, `%s` marks where the page goes, and of several browsers separated by `:`, the first is used.
- **Otherwise the system's choice.** awshop asks `xdg-open` (Linux) or `open` (macOS) to open the page, and they use the default program for HTML files. That is normally your default browser. On Linux, check it with `xdg-mime query default text/html`, and change it with `xdg-mime default <browser>.desktop text/html`.
- **No browser,** as over SSH: awshop names the page to open by hand. It holds a sign-in token, so don't copy it elsewhere.

## Several accounts at once

By default, the console allows one signed-in account per browser. Signing in to a second one shows "You must first log out before logging into a different AWS account".

To have several, turn on multi-session once per browser: choose your account name at the top right in the console, then **Turn on multi-session**, or choose **Enable multi-session** on console.aws.amazon.com. The console then keeps up to five sign-ins side by side, each in its own tab, as [AWS describes](https://docs.aws.amazon.com/awsconsolehelpdocs/latest/gsg/multisession.html).

## Safety

- **No token on a command line.** A sign-in link holds the token, and command lines are visible to other users of the machine. The browser gets the path of the page instead.
- **A private page.** The page is `$XDG_RUNTIME_DIR/awshop/console.html` (or under `$TMPDIR/awshop-<uid>/`), readable only by you. Each sign-in replaces it, and the token in it expires within 15 minutes.

## Requirements

- **Temporary credentials:** the console needs a session token, so profiles from SSO or with a role work. A profile with long-term access keys (an IAM user) can't sign in this way; awshop says so.
- **AWS CLI 2.9 or newer,** for `export-credentials`.
- **Partitions:** GovCloud (`us-gov-*`) and China (`cn-*`) regions use their own sign-in and console addresses.
