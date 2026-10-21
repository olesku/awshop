# Add eval "$(awshop init bash)" to ~/.bashrc, or source this file. Requires Bash 4.2 or newer.
# No top-level return: under eval in ~/.bashrc it would end the rest of that file.
if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 2) )); then
  printf '%s\n' "awshop: the bash integration requires Bash 4.2 or newer." >&2
else
  # Run awshop and evaluate the code it writes to fd 3. Stdout and stderr stay normal,
  # so help, init and other output work as usual.
  awshop() {
    local pid=$BASHPID code
    { code="$(AWSHOP_SHELL_PID=$pid command awshop --shell bash "$@" 3>&1 1>&4 4>&-)"; } 4>&1 || return
    eval "$code"
  }

  # Remove this shell's sessions when it exits. awshop also sweeps sessions of dead shells.
  _awshop_exit() { command awshop __session-cleanup "$BASHPID" 2>/dev/null; }

  # Bash has a single EXIT trap; keep any existing one and run it after cleanup.
  _awshop_install_exit_trap() {
    local existing
    existing="$(trap -p EXIT)"
    [[ "$existing" == *_awshop_exit* ]] && return 0
    if [[ -n "$existing" ]]; then
      eval "set -- $existing"
      trap -- "_awshop_exit; $3" EXIT
    else
      trap -- _awshop_exit EXIT
    fi
  }
  _awshop_install_exit_trap

  # Set REPLY to the current context from the first kubeconfig file that sets one, as
  # kubectl does. Reads files with builtins only; prompts run this on every line.
  _awshop_kube_context() {
    REPLY=""
    local pattern=$'(^|\n)[ \t]*"?current-context"?[ \t]*:[ \t]*["\']?([^"\',\r\n]*)'
    local kubeconfig content
    local -a kubeconfigs
    IFS=: read -r -a kubeconfigs <<< "${KUBECONFIG:-$HOME/.kube/config}"
    for kubeconfig in "${kubeconfigs[@]}"; do
      [[ -n "$kubeconfig" && -f "$kubeconfig" && -r "$kubeconfig" ]] || continue
      content=""
      read -r -d '' content < "$kubeconfig"
      [[ "$content" =~ $pattern ]] || continue
      REPLY="${BASH_REMATCH[2]%"${BASH_REMATCH[2]##*[^[:space:]]}"}"
      [[ -n "$REPLY" ]] && return 0
    done
    return 1
  }

  # Segments come from AWSHOP_PROMPT_SEGMENTS (default "aws"): aws, region and eks.
  # The EKS context shows its last part unless AWSHOP_PROMPT_EKS=full. A trailing *
  # means KUBECONFIG points at an awshop session file.
  awshop_prompt() {
    local profile="${AWS_PROFILE:-${AWS_DEFAULT_PROFILE-}}"
    [[ -n "$profile" ]] || return 0

    local -a segments output
    read -r -a segments <<< "${AWSHOP_PROMPT_SEGMENTS:-aws}"
    local region="${AWS_REGION:-${AWS_DEFAULT_REGION-}}" segment value
    local with_aws=0 with_region=0
    for segment in "${segments[@]}"; do
      [[ "$segment" == aws ]] && with_aws=1
      [[ "$segment" == region ]] && with_region=1
    done
    for segment in "${segments[@]}"; do
      case "$segment" in
        aws)
          value="aws:$profile"
          (( with_region )) && [[ -n "$region" ]] && value+="@$region"
          output+=("$value")
          ;;
        region)
          (( with_aws )) || [[ -z "$region" ]] || output+=("region:$region")
          ;;
        eks)
          _awshop_kube_context || continue
          value="$REPLY"
          [[ "${AWSHOP_PROMPT_EKS-}" == full ]] || value="${value##*/}"
          [[ "${KUBECONFIG-}" == */awshop*/sessions/*/config ]] && value+="*"
          output+=("eks:$value")
          ;;
      esac
    done
    local IFS=' '
    printf '%s' "${output[*]}"
  }
fi
