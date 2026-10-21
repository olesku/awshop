#!/bin/sh
# Install awshop from the GitHub releases:
#   curl -fsSL https://raw.githubusercontent.com/olesku/awshop/main/setup.sh | bash
#
# AWSHOP_VERSION       a release such as v0.1.0 (default: the latest release)
# AWSHOP_INSTALL_DIR   where the binary goes (default: ~/.local/bin)
# AWSHOP_DOWNLOAD_URL  where to download from instead, such as a mirror
#
# The checksum guards against a damaged or cut-off download. It comes from the same
# place as the archive, so it is not a signature.
set -eu

repository=olesku/awshop

fail() {
  printf 'awshop setup: %s\n' "$1" >&2
  exit 1
}

# Everything runs from main, so a partial download through a pipe runs nothing.
main() {
  case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) fail "unsupported system $(uname -s); awshop runs on Linux and macOS." ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=amd64 ;;
    aarch64 | arm64) arch=arm64 ;;
    *) fail "unsupported architecture $(uname -m); awshop is built for amd64 and arm64." ;;
  esac
  command -v curl >/dev/null || fail "curl is required."
  if command -v sha256sum >/dev/null; then
    sha256() { sha256sum "$1" | cut -d ' ' -f 1; }
  elif command -v shasum >/dev/null; then
    sha256() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
  else
    fail "sha256sum or shasum is required to verify the download."
  fi

  version=${AWSHOP_VERSION:-latest}
  if [ "$version" = latest ]; then
    base="https://github.com/$repository/releases/latest/download"
  else
    # Releases are tagged vX.Y.Z; accept the version without the v too.
    case $version in v*) ;; *) version="v$version" ;; esac
    base="https://github.com/$repository/releases/download/$version"
  fi
  base=${AWSHOP_DOWNLOAD_URL:-$base}
  archive="awshop-$os-$arch.tar.gz"
  install_dir=${AWSHOP_INSTALL_DIR:-$HOME/.local/bin}

  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  download() {
    curl -fsSL --proto '=https,file' --tlsv1.2 -o "$work/$1" "$base/$1" || fail "could not download $base/$1."
  }
  download "$archive"
  download checksums.txt
  expected=$(awk -v name="$archive" '$2 == name { print $1 }' "$work/checksums.txt")
  [ -n "$expected" ] || fail "checksums.txt has no entry for $archive."
  [ "$(sha256 "$work/$archive")" = "$expected" ] || fail "checksum mismatch for $archive."

  tar -xzf "$work/$archive" -C "$work"
  mkdir -p "$install_dir"
  # Replace the binary in one step, so a running awshop keeps its old file.
  cp "$work/awshop-$os-$arch/awshop" "$install_dir/.awshop.new"
  chmod 755 "$install_dir/.awshop.new"
  mv "$install_dir/.awshop.new" "$install_dir/awshop"
  printf 'Installed %s\n' "$("$install_dir/awshop" --version 2>/dev/null || printf 'awshop')"
  printf 'Location: %s/awshop\n' "$install_dir"

  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) printf '\n%s is not in PATH. Add it to your shell rc file:\n  export PATH="%s:$PATH"\n' "$install_dir" "$install_dir" ;;
  esac
  case "$(basename "${SHELL:-}")" in
    zsh) setup='eval "$(awshop init zsh)"' rc='~/.zshrc' ;;
    bash) setup='eval "$(awshop init bash)"' rc='~/.bashrc' ;;
    fish) setup='awshop init fish | source' rc='~/.config/fish/config.fish' ;;
    *) setup='eval "$(awshop init zsh)"' rc='your shell rc file' ;;
  esac
  printf '\nTo let awshop change your shell, add this to %s and open a new shell:\n  %s\n' "$rc" "$setup"
}

main "$@"
