# Add eval "$(awshop init zsh)" to ~/.zshrc, or source this file.
zmodload zsh/system

# Run awshop and evaluate the code it writes to fd 3. Stdout and stderr stay normal,
# so help, init and other output work as usual.
awshop() {
  local pid=${sysparams[pid]} code
  { code="$(AWSHOP_SHELL_PID=$pid command awshop --shell zsh "$@" 3>&1 1>&4 4>&-)"; } 4>&1 || return
  eval "$code"
}

# Remove this shell's sessions when it exits. awshop also sweeps sessions of dead shells.
_awshop_exit() { command awshop __session-cleanup ${sysparams[pid]} 2>/dev/null }
autoload -Uz add-zsh-hook
add-zsh-hook zshexit _awshop_exit

# Set REPLY to the current context from the first kubeconfig file that sets one, as
# kubectl does. Reads files with builtins only; prompts run this on every line.
_awshop_kube_context() {
  emulate -L zsh
  REPLY=""
  local pattern=$'(^|\n)[ \t]*"?current-context"?[ \t]*:[ \t]*["\']?([^"\',\r\n]*)'
  local kubeconfig content
  for kubeconfig in "${(@s/:/)${KUBECONFIG:-$HOME/.kube/config}}"; do
    [[ -n "$kubeconfig" && -f "$kubeconfig" && -r "$kubeconfig" ]] || continue
    content=""
    read -r -d '' content < "$kubeconfig"
    [[ "$content" =~ $pattern ]] || continue
    REPLY="${match[2]%"${match[2]##*[^[:space:]]}"}"
    [[ -n "$REPLY" ]] && return 0
  done
  return 1
}

# Segments come from AWSHOP_PROMPT_SEGMENTS (default "aws"): aws, region and eks.
# The EKS context shows its last part unless AWSHOP_PROMPT_EKS=full. A trailing *
# means KUBECONFIG points at an awshop session file.
awshop_prompt() {
  emulate -L zsh
  local profile="${AWS_PROFILE:-${AWS_DEFAULT_PROFILE-}}"
  [[ -n "$profile" ]] || return 0

  local -a segments output
  segments=(${=AWSHOP_PROMPT_SEGMENTS:-aws})
  local region="${AWS_REGION:-$AWS_DEFAULT_REGION}" segment value
  for segment in $segments; do
    case "$segment" in
      aws)
        value="aws:$profile"
        (( ${segments[(Ie)region]} )) && [[ -n "$region" ]] && value+="@$region"
        output+=("$value")
        ;;
      region)
        (( ${segments[(Ie)aws]} )) || [[ -z "$region" ]] || output+=("region:$region")
        ;;
      eks)
        _awshop_kube_context || continue
        value="$REPLY"
        [[ "$AWSHOP_PROMPT_EKS" == full ]] || value="${value##*/}"
        [[ "$KUBECONFIG" == */awshop*/sessions/*/config ]] && value+="*"
        output+=("eks:$value")
        ;;
    esac
  done
  print -rn -- "${(j: :)output}"
}
