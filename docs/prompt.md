# Prompt

The [zsh, Bash or fish integration](../README.md#shell-integration) defines `awshop_prompt`. By default it prints `aws:<profile>` when `AWS_PROFILE` or `AWS_DEFAULT_PROFILE` is set, and nothing otherwise.

```zsh
# zsh (~/.zshrc)
setopt PROMPT_SUBST
PROMPT='%n@%m %~ $(awshop_prompt) %# '
```

With Oh My Zsh and a theme that sets `RPROMPT`, keep the theme's prompt and add awshop before it. Put these lines after `source "$ZSH/oh-my-zsh.sh"` in `~/.zshrc`, replacing any other `awshop init zsh` line:

```zsh
eval "$(command awshop init zsh)"
AWSHOP_PROMPT_SEGMENTS="aws region eks"
RPROMPT='$(awshop_prompt) '"$RPROMPT"
```

Oh My Zsh enables `PROMPT_SUBST`, so the values update each time the prompt is drawn. Themes that manage their own prompt, such as Powerlevel10k, can use their AWS and Kubernetes segments instead.

```bash
# Bash (~/.bashrc)
PS1='\u@\h \w $(awshop_prompt) \$ '
```

```fish
# fish: add (awshop_prompt) to your fish_prompt function
function fish_prompt
    printf '%s %s> ' (prompt_pwd) (awshop_prompt)
end
```

Choose segments with `AWSHOP_PROMPT_SEGMENTS` (in fish: `set -g AWSHOP_PROMPT_SEGMENTS "aws region eks"`):

```sh
AWSHOP_PROMPT_SEGMENTS="aws region eks"
# aws:dev@eu-north-1 eks:dev-1*
```

| Segment | Shows |
| --- | --- |
| `aws` | `AWS_PROFILE`, else `AWS_DEFAULT_PROFILE`. This is the default segment. |
| `region` | `AWS_REGION` or `AWS_DEFAULT_REGION`, when set: added to `aws` as `@region`, or shown as `region:<region>` without the `aws` segment. |
| `eks` | The current Kubernetes context. Only the last part of the name is shown, or all of it with `AWSHOP_PROMPT_EKS=full`. `*` means `KUBECONFIG` points at an `awshop eks` session file, judged by its path, so subshells show it too. |

- **No profile:** all segments are hidden when neither `AWS_PROFILE` nor `AWS_DEFAULT_PROFILE` is set.
- **Empty segments** are left out.
- **EKS context:** `eks` reads `current-context` from the first file in `KUBECONFIG` (or `~/.kube/config`) that sets one, as kubectl does, so it also follows `kubectl config use-context`.
- **Speed:** the prompt uses only shell builtins and never runs `aws` or `kubectl`.

Starship and Powerlevel10k have their own AWS and Kubernetes modules, which work with awshop as they are.
