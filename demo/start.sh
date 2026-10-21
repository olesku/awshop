#!/bin/sh
# Start a zsh for recording the demo: an empty environment, a throwaway HOME with
# made-up accounts, and the fake AWS tools in demo/bin. Nothing of yours is read.
set -eu
demo=$(cd "$(dirname "$0")" && pwd)
repo=$(dirname "$demo")
home=$(mktemp -d "${TMPDIR:-/tmp}/awshop-demo.XXXXXX")
mkdir -p "$home/.aws" "$home/run" "$home/state"
chmod 700 "$home/run"
cat > "$home/.aws/config" <<'CONFIG'
[sso-session demo]
sso_start_url = https://example.awsapps.com/start
sso_region = eu-north-1
sso_registration_scopes = sso:account:access

[profile platform-dev]
sso_session = demo
sso_account_id = 111111111111
sso_role_name = AdministratorAccess
region = eu-north-1

[profile platform-staging]
sso_session = demo
sso_account_id = 222222222222
sso_role_name = AdministratorAccess
region = eu-north-1

[profile platform-prod]
sso_session = demo
sso_account_id = 333333333333
sso_role_name = ReadOnlyAccess
region = eu-north-1

[profile data-dev]
sso_session = demo
sso_account_id = 444444444444
sso_role_name = AdministratorAccess
region = eu-west-1

[profile data-prod]
sso_session = demo
sso_account_id = 555555555555
sso_role_name = ReadOnlyAccess
region = eu-west-1

[profile sandbox]
sso_session = demo
sso_account_id = 666666666666
sso_role_name = AdministratorAccess
region = eu-north-1
CONFIG
# Skip the system's zsh files after zshenv, and never sync with a real SSO portal.
printf 'setopt no_global_rcs\n' > "$home/.zshenv"
mkdir -p "$home/config/awshop"
printf '[sso]\nsync = false\n' > "$home/config/awshop/config.toml"
cat > "$home/.zshrc" <<ZSHRC
eval "\$(awshop init zsh)"
setopt PROMPT_SUBST
AWSHOP_PROMPT_SEGMENTS="aws eks"
PROMPT='%F{39}~%f %F{214}\$(awshop_prompt)%f %# '
ZSHRC
cd "$home"
exec env -i \
  HOME="$home" ZDOTDIR="$home" TERM="${TERM:-xterm-256color}" LANG=C.UTF-8 \
  PATH="$demo/bin:$repo/target/release:/usr/bin:/bin" \
  XDG_RUNTIME_DIR="$home/run" XDG_STATE_HOME="$home/state" XDG_CONFIG_HOME="$home/config" \
  AWSHOP_DEMO_STATE="$home" SHELL=/bin/zsh \
  zsh -i
