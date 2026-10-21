# EKS plugin

`awshop eks` lists the EKS clusters for your AWS profile and sets up kubectl access. It writes either a temporary kubeconfig for the current shell or your saved kubeconfig file.

```sh
awshop eks                  # choose a cluster, start a session in this shell
awshop eks leave            # end the session and restore KUBECONFIG
awshop eks --saved          # write to your kubeconfig file instead
awshop eks --profile dev --region eu-north-1 dev-1
```

The plugin uses your active profile, or the profile whose menu you chose EKS in. Through the selector, a successful action also sets `AWS_PROFILE` to the profile it used. The region comes from `--region`, `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile, in that order. If none is set, it asks.

1. **Pick a cluster.** The list shows each cluster's Kubernetes version, such as `v1.31`, and a status other than `ACTIVE`, such as `UPDATING`. `*` marks the cluster of kubectl's current context, across all files in `KUBECONFIG`, and the list starts there. Versions arrive a moment after the list; without `eks:DescribeCluster`, it shows none.
2. **Review the setup.** It shows the profile, region, scope, authentication, context name, lookup role and, for Saved config, the kubeconfig file. The text below the menu explains the highlighted option.
3. **Run the action.** Select **Start EKS session**, or **Configure and activate** / **Update and activate** for Saved config.

In the cluster list, `Ctrl-P` changes the profile and `Ctrl-R` the region, for this run only, and `Ctrl-L` reloads the list. In every menu, ← or Escape goes back and `Ctrl-C` exits; in the first menu, only Escape leaves.

## k9s

When [k9s](https://k9scli.io) is in `PATH`, the setup screen has a second action below the first: **Start EKS session and open k9s**, or **Configure and open k9s** / **Update and open k9s** for Saved config.
- **What it does:** the same setup, then k9s opens on the new context, with the whole terminal.
- **Afterwards:** quitting k9s returns you to the shell with the setup in place. A session stays active, so `kubectl` works on the same cluster, and `awshop eks leave` ends it as usual.

## Scope

**This shell** is the default, unless `scope = "saved"` is set in [config.toml](config.md#scope). It writes a private temporary kubeconfig and points `KUBECONFIG` at it, and your saved kubeconfig is not touched.
- **Location:** `$XDG_RUNTIME_DIR/awshop/sessions/eks-*/config`, or `$TMPDIR/awshop-<uid>/sessions/` when `XDG_RUNTIME_DIR` isn't set. The directories are readable only by you.
- **Leaving:** `awshop eks leave` restores the previous value, also when `KUBECONFIG` was unset. A `KUBECONFIG` that wasn't exported counts as unset. If you changed `KUBECONFIG` yourself since, it is kept.
- **Replacing:** a new session replaces the current one only after it succeeds.
- **Cleanup:** the files are removed when you leave or the shell exits. If the shell is killed, the next awshop run through the shell function, in any shell, removes them, and the system clears `XDG_RUNTIME_DIR` when you log out.
- **Requirement:** this needs the zsh, Bash or fish integration. A readonly `KUBECONFIG` makes the shell report an error, and the session isn't used.

**Saved config** (`--saved`, `scope = "saved"`, or **Change scope**) writes to a kubeconfig file and sets its current context.
- **File:** the default is the first path in `KUBECONFIG` from before any session, also in a shell started from a session shell, or `~/.kube/config`. If you choose another file, use it with `kubectl --kubeconfig <path>` or add it to `KUBECONFIG`.
- **Active session:** a session in this shell ends when a Saved config setup succeeds.

## Authentication and names

- **Authentication:** kubectl authenticates with the AWS profile, a saved named role, or a role ARN you enter (`--role-arn`). **Lookup role** (`--assume-role-arn`) only affects how cluster information is retrieved.
- **Context name:** the default is `<profile>/<region>/<cluster>`. The kubeconfig user gets the same name, so contexts for different profiles keep separate authentication. A name already used by another cluster or profile is refused, so its authentication is never overwritten.
- **Writing:** the AWS CLI's [`update-kubeconfig`](https://docs.aws.amazon.com/cli/latest/reference/eks/update-kubeconfig.html) does the actual write.
- **Remembered settings:** settings that worked are remembered per profile and cluster ARN, as are named roles. Temporary paths and credentials are never saved. See [State](config.md#state).
- **Defaults:** set the default scope and predefined roles in [config.toml](config.md).

## Permissions

Discovery uses the selected profile, so pick a profile in the cluster's account. The profile needs `sts:GetCallerIdentity` and permission to list and describe EKS clusters. A kubeconfig entry doesn't give Kubernetes access by itself. `--no-login` skips the SSO check, but discovery still needs valid credentials.
