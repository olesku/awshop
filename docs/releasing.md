# Releasing

Releases are built by GitHub Actions on GitHub's own runners. Pushing a tag `vX.Y.Z` builds and publishes everything.

1. **Bump the version** in `Cargo.toml` (`version = "X.Y.Z"`), run `cargo build` so `Cargo.lock` follows, and merge that to `main`.
2. **Tag and push:**
   ```sh
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
3. **Wait for the Release workflow.** It publishes a GitHub release with notes generated from the merged pull requests.

## What the workflow does

| Job | Does |
| --- | --- |
| Check the tag | Fails unless the tag equals `v` plus the version in `Cargo.toml` |
| Build | Builds `--release --locked` for four targets and packs `awshop`, `README.md` and `LICENSE` |
| Publish the release | Writes `checksums.txt` (SHA-256) and creates the release with all files |

| Asset | Target | Runner |
| --- | --- | --- |
| `awshop-linux-amd64.tar.gz` | `x86_64-unknown-linux-musl` (static) | `ubuntu-latest` |
| `awshop-linux-arm64.tar.gz` | `aarch64-unknown-linux-musl` (static) | `ubuntu-24.04-arm` |
| `awshop-darwin-amd64.tar.gz` | `x86_64-apple-darwin` | `macos-latest` |
| `awshop-darwin-arm64.tar.gz` | `aarch64-apple-darwin` | `macos-latest` |

- **Stable names:** asset names carry no version, so `releases/latest/download/<asset>` always points at the newest release. `setup.sh` relies on that.
- **Static Linux binaries:** musl builds run on any distribution, without a matching glibc.
- **No third-party actions:** the workflows use only `actions/*` and the `rustup` and `gh` that GitHub's runners include.
- **macOS:** the binaries aren't signed. Downloads with `curl`, as in `setup.sh`, aren't quarantined, so Gatekeeper doesn't block them; a binary downloaded with a browser needs `xattr -d com.apple.quarantine awshop`.

## Fixing a failed release

If a build fails, fix it on `main`, then move the tag and push it again:

```sh
git tag -d vX.Y.Z && git push origin :refs/tags/vX.Y.Z
git tag vX.Y.Z && git push origin vX.Y.Z
```

Delete a half-created GitHub release first, or `gh release create` refuses the tag.
