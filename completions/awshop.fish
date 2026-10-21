# Add awshop init fish | source to ~/.config/fish/config.fish, or source this file.

# Run awshop and evaluate the code it writes to fd 3. fish cannot point a descriptor
# into a command substitution, so the code goes through a private temporary file.
# Stdout and stderr stay normal, so help, init and other output work as usual.
function awshop
    set -l tmp (mktemp); or return
    AWSHOP_SHELL_PID=$fish_pid command awshop --shell fish $argv 3>$tmp
    set -l rc $status
    read -lz code <$tmp
    rm -f $tmp
    test $rc -eq 0; or return $rc
    eval $code
end

# Remove this shell's sessions when it exits. awshop also sweeps sessions of dead shells.
function _awshop_exit --on-event fish_exit
    command awshop __session-cleanup $fish_pid 2>/dev/null
end

# Print the current context from the first kubeconfig file that sets one, as kubectl
# does. Uses builtins only; prompts run this on every line.
function _awshop_kube_context
    set -l kubeconfigs $HOME/.kube/config
    test -n "$KUBECONFIG"; and set kubeconfigs (string split : -- $KUBECONFIG)
    for kubeconfig in $kubeconfigs
        test -n "$kubeconfig" -a -f "$kubeconfig" -a -r "$kubeconfig"; or continue
        set -l context (string match -rg '^[ \t]*"?current-context"?[ \t]*:[ \t]*["\']?([^"\',\r]*)' <$kubeconfig | string trim)
        test -n "$context[1]"; or continue
        printf '%s\n' $context[1]
        return 0
    end
    return 1
end

# Segments come from AWSHOP_PROMPT_SEGMENTS (default "aws"): aws, region and eks.
# The EKS context shows its last part unless AWSHOP_PROMPT_EKS=full. A trailing *
# means KUBECONFIG points at an awshop session file.
function awshop_prompt
    set -l profile $AWS_PROFILE
    test -n "$profile"; or set profile $AWS_DEFAULT_PROFILE
    test -n "$profile"; or return 0

    set -l segments aws
    test -n "$AWSHOP_PROMPT_SEGMENTS"; and set segments (string split -n ' ' -- $AWSHOP_PROMPT_SEGMENTS)
    set -l region $AWS_REGION
    test -n "$region"; or set region $AWS_DEFAULT_REGION
    set -l output
    for segment in $segments
        switch $segment
            case aws
                set -l value aws:$profile
                contains region $segments; and test -n "$region"; and set value $value@$region
                set -a output $value
            case region
                contains aws $segments; or test -z "$region"; or set -a output region:$region
            case eks
                set -l value (_awshop_kube_context); or continue
                test "$AWSHOP_PROMPT_EKS" = full; or set value (string replace -r '.*/' '' -- $value)
                string match -q '*/awshop*/sessions/*/config' -- "$KUBECONFIG"; and set value "$value*"
                set -a output eks:$value
        end
    end
    string join ' ' -- $output
end
