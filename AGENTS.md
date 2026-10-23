# AGENTS.md

Guidance for coding agents working on awshop. Read [README.md](README.md) first for what the tool does; this file is about how the code is built and kept.

## Project

awshop is a Rust terminal UI for AWS profiles, with plugins for EKS and SSM. The binary owns all logic and state; the shell functions in `completions/` only run code the binary sends them.

| Path | Contents |
| --- | --- |
| `src/main.rs` | Startup, the fd 3 code channel, command routing, `--help` |
| `src/selector.rs` | Profile selector, the profile's menu, recent profiles, logo |
| `src/plugin.rs` | Plugin host: context, resource list, setup screen, running programs |
| `src/ui.rs` | Menus, text input, drawing, the held screen and Ctrl-C handling |
| `src/session.rs` | Per-shell session directories, manifests, cleanup and sweep |
| `src/shell.rs` | Ops (`Set`, `Unset`, `Say`), rendering to shell code, quoting, fd 3 |
| `src/sso.rs` | Profiles from the SSO portal, written to awshop's block of the AWS config |
| `src/console.rs` | Signing in to the AWS console as a profile |
| `src/aws.rs` | AWS config parsing, SSO login checks, executable lookup |
| `src/config.rs`, `src/preferences.rs` | `config.toml` (read only) and the state file |
| `src/cache.rs` | Resource lists from earlier runs, shown while a fresh list loads |
| `src/init.rs`, `src/profiles.rs` | `awshop init`; `awshop add` and `--doctor` |
| `crates/awshop-plugin` | The plugin contract: `Plugin`, `Context`, `Resource`, `Setup`, `Outcome` |
| `crates/awshop-eks`, `crates/awshop-ssm` | The built-in plugins |
| `completions/` | zsh, Bash and fish functions, embedded in the binary |
| `tests/*.py` | Interactive tests: the real binary in a pseudoterminal, with fakes |
| `demo/` | Made-up accounts and fake AWS tools for recording `docs/demo.gif` |
| `docs/` | Details per topic; README stays an overview |

## Commands

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build && python3 tests/eks_pty.py && python3 tests/session_pty.py && python3 tests/sso_pty.py
```

All of these must pass before work is done. CI runs the same. `tests/session_pty.py` runs for zsh, Bash and fish; install them to test all three.

Re-record the demo when visible UI changes: `cargo build --release && vhs demo/awshop.tape`, then check the frames for anything real.

## Rules that keep the design safe

- **Shell code:** the shell functions evaluate only what the binary writes to fd 3. Never print shell code to stdout for them. Every value in that code goes through `shell::quote` (via `shell::render`), message text included.
- **One place for each rule:** quoting and variable-name checks live only in `shell.rs`; session paths and manifests only in `session.rs`; the SSO block of the AWS config only in `sso.rs`.
- **Deleting files:** only known file names inside awshop's own directories, never recursively, never outside the session root.
- **The AWS config:** awshop writes only the marked `# BEGIN awshop` block. Everything outside it is the user's and stays byte for byte.
- **Credentials:** never put tokens or keys in arguments, logs, preferences or test output. The SSO token stays in memory and goes to the portal over HTTPS.
- **Plugins:** they discover, prepare and execute; they never read the terminal or write to stdout. Interactive programs (k9s, an SSM shell) are returned as commands for the host to run. The host owns sessions and the environment.

## Code style

- **No bloat:** no speculative abstractions, no dead code. A helper used once is inlined. Remove what a change makes unnecessary.
- **Errors, not panics:** no `unwrap` or `expect` outside tests. Use `awshop_plugin::Result` with string errors via `.into()` and `format!`. Check the return value of every `libc` call.
- **Comments:** short doc comments on functions, structs, enums and traits. Comment logic that isn't obvious; never repeat the code.
- **Blank lines:** rustfmt keeps them but never adds them. Put one between logical steps in a function: after guards and early returns, before a comment that starts a new step, and around a loop or match that is a step of its own. None right after `{` or before `}`, and none between lines that belong together.
- **Formatting:** `rustfmt.toml` sets two-space indentation and 120 columns. Run `cargo fmt`.
- **Messages:** terse and specific. No "Checking…" or "Loading…" lines; a waiting screen says "Waiting for AWS…". Menus show no description text that only repeats the row.
- **Language:** code, comments, docs and messages are in English.

## Tests

- **Behavior gets a test.** New behavior and fixed bugs get an interactive test in `tests/` or a unit test; prefer one that would fail without the change.
- **Fakes, never AWS:** the Python tests use fake `aws`, `kubectl`, `session-manager-plugin` and `xdg-open` programs, plus `k9s` where a test asks for it (the `MOCK` in `tests/eks_pty.py`), and a fake SSO portal and sign-in endpoint (`tests/sso_pty.py`, via `AWSHOP_SSO_PORTAL` and `AWSHOP_SIGNIN`). They run in a temporary `HOME` with an environment built from scratch, and must never read or write the user's real config.
- **Room for slow runners:** time limits in tests leave several times the time the step needs.
- **Shell bodies:** `session_pty.py` bodies are POSIX syntax for zsh and Bash, with a separate fish body.

## Documentation

- **Update docs with behavior:** README for the overview, `docs/*.md` for details. Keep sections short, with bulleted lists that start with a bold lead, and tables.
- **No real names:** examples use made-up accounts (`platform-dev`, `123456789012`, `example.awsapps.com`); never anything from a real AWS config.

## Workflow

- **Commits:** don't commit, push or tag unless asked. The owner commits.
- **Releases:** tags `vX.Y.Z` build release binaries in GitHub Actions; see [docs/releasing.md](docs/releasing.md).
