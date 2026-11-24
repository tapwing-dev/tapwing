#!/bin/sh
# Installs a Tapwing release (a tarball built by scripts/dist.sh, or a GitHub Release).
#
#   curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash
#   curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash -s 0.1.0
#   curl -fsSL …/install.sh | sh -s -- --version 0.1.0 --prefix DIR --bin-dir DIR
#   install.sh [SOURCE|VERSION] [--prefix DIR] [--bin-dir DIR] [--sha256 HEX] [--no-modify-path]
#   install.sh --version VERSION|latest [--repo OWNER/REPO] [--target TRIPLE] [--prefix DIR] [--bin-dir DIR] [--sha256 HEX]
#   install.sh --print-target
#   install.sh --uninstall [--prefix DIR] [--bin-dir DIR] [--no-modify-path]
#
# SOURCE is a tapwing-<version>-<target>.tar.gz, an http(s) URL to one, or an
# unpacked release directory. A positional VERSION (latest, X.Y.Z, vX.Y.Z) is
# shorthand for --version VERSION when no local file of that name exists.
# Without SOURCE or --version, the unpacked release this script ships in is
# installed; when the script is not inside one (piped into bash/sh, or run from
# a checkout), the latest GitHub Release is installed.
# With --version, the host OS/arch is detected via uname (plus Rosetta detection
# on macOS) and the matching GitHub Release tarball (+ .sha256) is downloaded;
# it is not installed unless that .sha256 (or --sha256) matches. --version latest
# follows the redirect of https://github.com/<repo>/releases/latest (no GitHub
# API, no rate limit). A tarball is checked against --sha256, else against
# <tarball>.sha256 when that exists (next to a local file, or at <URL>.sha256).
# Needs only curl or wget, tar, and sha256sum or shasum.
#
# Layout (default prefix ~/.tapwing, default bin dir ~/.local/bin):
#   <prefix>/versions/<version>/{bin/tapwing,share/tapwing/…}
#   <prefix>/current -> versions/<version>
#   <bin dir>/tapwing -> <prefix>/current/bin/tapwing
# When <bin dir> is not on PATH, the script adds an idempotent PATH block to the
# user's shell config (~/.zshrc, ~/.bashrc, ~/.bash_profile, config.fish, or
# ~/.profile), which --uninstall removes cleanly. Pass --no-modify-path (or
# TAPWING_NO_MODIFY_PATH=1) to skip shell config edits.
# Re-running installs over the same version or upgrades to a new one; older
# versions are removed. Tapwing finds its bundled native-lane drivers through
# the symlinks. The driver cache Tapwing itself writes (~/.tapwing/ios-native-driver/,
# and a ~/.tapwing/native-driver/ left by older versions) is left alone, also by --uninstall.
#
# Environment variables:
#   TAPWING_PREFIX         default --prefix (default: ~/.tapwing)
#   TAPWING_BIN_DIR        default --bin-dir (default: ~/.local/bin)
#   TAPWING_VERSION        default --version (overridden by flags or positional arg)
#   TAPWING_REPO           default --repo (default: tapwing-dev/tapwing)
#   TAPWING_NO_MODIFY_PATH set to 1 to skip editing shell config files
set -eu

prefix="${TAPWING_PREFIX:-${HOME:?HOME is not set}/.tapwing}"
bin_dir="${TAPWING_BIN_DIR:-$HOME/.local/bin}"
source=""
expected_sha=""
want_version=""
env_version="${TAPWING_VERSION:-}"
repo="${TAPWING_REPO:-tapwing-dev/tapwing}"
want_target=""
print_target=0
uninstall=0
no_modify_path="${TAPWING_NO_MODIFY_PATH:-0}"
downloader=""

die() { echo "install.sh: $*" >&2; exit 1; }
note() { echo "install.sh: $*" >&2; }

# Also works when the script is piped into bash/sh (no file to read the header from).
usage() {
  cat <<'EOF'
Installs a Tapwing release.

  curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash
  curl -fsSL https://raw.githubusercontent.com/tapwing-dev/tapwing/main/scripts/install.sh | bash -s 0.1.0
  curl -fsSL .../install.sh | sh -s -- [OPTIONS]
  install.sh [SOURCE|VERSION] [--prefix DIR] [--bin-dir DIR] [--sha256 HEX] [--no-modify-path]
  install.sh --version VERSION|latest [--repo OWNER/REPO] [--target TRIPLE] [--prefix DIR] [--bin-dir DIR] [--sha256 HEX]
  install.sh --print-target
  install.sh --uninstall [--prefix DIR] [--bin-dir DIR] [--no-modify-path]

SOURCE|VERSION    a tapwing-<version>-<target>.tar.gz, an http(s) URL to one, an
                  unpacked release directory, or a version shorthand (latest, X.Y.Z, vX.Y.Z).
                  Default: the unpacked release this script ships in, else the latest GitHub Release.
--version VER     download a GitHub Release (VER or vVER, or latest) for this host;
                  it is installed only if its .sha256 (or --sha256) matches
--repo OWNER/REPO GitHub repository for --version (default: $TAPWING_REPO or tapwing-dev/tapwing)
--target TRIPLE   override the detected target for --version
--prefix DIR      install root (default: $TAPWING_PREFIX or ~/.tapwing)
--bin-dir DIR     where the tapwing symlink goes (default: $TAPWING_BIN_DIR or ~/.local/bin)
--sha256 HEX      expected SHA-256 of the tarball
--no-modify-path  do not edit shell rc files to add --bin-dir to PATH
--print-target    print the detected target triple and exit
--uninstall       remove the installation (driver caches in ~/.tapwing stay)
EOF
}

is_version_spec() {
  case "$1" in
    latest) return 0 ;;
  esac
  _v=${1#v}
  _core=${_v%%-*}
  _rest=""
  case "$_v" in
    *-*)
      _rest=${_v#*-}
      [ -n "$_rest" ] || return 1
      ;;
  esac
  case "$_rest" in
    *[!A-Za-z0-9._+-]*) return 1 ;;
  esac
  _maj=${_core%%.*}
  _after_maj=${_core#*.}
  [ "$_after_maj" != "$_core" ] || return 1
  _min=${_after_maj%%.*}
  _pat=${_after_maj#*.}
  [ "$_pat" != "$_after_maj" ] || return 1
  case "$_maj" in ""|*[!0-9]*) return 1 ;; esac
  case "$_min" in ""|*[!0-9]*) return 1 ;; esac
  case "$_pat" in ""|*[!0-9]*) return 1 ;; esac
  return 0
}

detect_host_targets() {
  os=$(uname -s)
  arch=$(uname -m)
  rosetta_arm64=0
  # In a Rosetta-translated shell on Apple Silicon, uname -m reports x86_64.
  # Probe sysctl so we install the native arm64 build instead of running under Rosetta.
  if [ "$os" = "Darwin" ] && [ "$arch" = "x86_64" ]; then
    sysctl_bin=$(command -v sysctl 2>/dev/null || echo /usr/sbin/sysctl)
    if [ -x "$sysctl_bin" ] && [ "$("$sysctl_bin" -n hw.optional.arm64 2>/dev/null || true)" = "1" ]; then
      arch="arm64"
      rosetta_arm64=1
    fi
  fi
  case "$os-$arch" in
    Darwin-arm64|Darwin-aarch64)
      default_target="aarch64-apple-darwin"
      # x86_64 macOS tarballs also run on Apple Silicon under Rosetta.
      host_targets="aarch64-apple-darwin x86_64-apple-darwin"
      ;;
    Darwin-x86_64)
      default_target="x86_64-apple-darwin"
      host_targets="x86_64-apple-darwin"
      ;;
    Linux-x86_64|Linux-amd64)
      default_target="x86_64-unknown-linux-gnu"
      host_targets="x86_64-unknown-linux-gnu"
      ;;
    Linux-aarch64|Linux-arm64)
      default_target="aarch64-unknown-linux-gnu"
      host_targets="aarch64-unknown-linux-gnu"
      ;;
    *)
      default_target=""
      host_targets=""
      ;;
  esac
}

require_downloader() {
  if [ -n "$downloader" ]; then
    return 0
  fi
  if command -v curl >/dev/null 2>&1; then
    downloader=curl
  elif command -v wget >/dev/null 2>&1; then
    downloader=wget
  else
    die "either curl or wget is required to download Tapwing"
  fi
}

download_file() {
  _url=$1
  _out=$2
  _quiet=${3:-0}
  require_downloader
  if [ "$downloader" = "curl" ]; then
    if [ "$_quiet" = 1 ]; then
      curl -fsSL -o "$_out" "$_url" 2>/dev/null
    else
      curl -fL --retry 3 --progress-bar -o "$_out" "$_url"
    fi
  else
    if [ "$_quiet" = 1 ]; then
      wget -q -O "$_out" "$_url" 2>/dev/null
    else
      wget -q --tries=3 -O "$_out" "$_url"
    fi
  fi
}

detect_shell_rc() {
  shell_kind=""
  shell_rc=""
  case "$(basename "${SHELL:-sh}")" in
    zsh)
      shell_kind=zsh
      shell_rc="${ZDOTDIR:-$HOME}/.zshrc"
      ;;
    bash)
      shell_kind=bash
      if [ "$(uname -s)" = "Darwin" ]; then
        if [ -f "$HOME/.bash_profile" ] && grep -qs '\.bashrc' "$HOME/.bash_profile" 2>/dev/null; then
          shell_rc="$HOME/.bashrc"
        else
          shell_rc="$HOME/.bash_profile"
        fi
      else
        shell_rc="$HOME/.bashrc"
      fi
      ;;
    fish)
      shell_kind=fish
      shell_rc="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
      ;;
    *)
      shell_kind=sh
      shell_rc="$HOME/.profile"
      ;;
  esac
}

# Resolves symlink chains so rewriting a stowed/symlinked rc file updates its target.
resolve_rc_path() {
  _p=$1
  _depth=0
  while [ -L "$_p" ] && [ "$_depth" -lt 40 ]; do
    _target=$(readlink "$_p") || return 1
    case "$_target" in
      /*) _p=$_target ;;
      *)
        _dir=$(dirname "$_p")
        if [ -d "$_dir" ]; then
          _dir=$(cd "$_dir" && pwd -P)
        fi
        _p="$_dir/$_target"
        ;;
    esac
    _depth=$((_depth + 1))
  done
  [ ! -L "$_p" ] || return 1
  printf '%s\n' "$_p"
}

esc_rc_path() {
  printf '%s' "$1" | sed 's/[\\"$`]/\\&/g'
}

format_rc_path() {
  _home_real=$(cd "$HOME" && pwd -P)
  case "$1" in
    "$_home_real") printf '$HOME' ;;
    "$_home_real"/*)
      _rel=$(awk -v p="$1" -v h="$_home_real/" 'BEGIN { print substr(p, length(h) + 1) }')
      printf '$HOME/%s' "$(esc_rc_path "$_rel")"
      ;;
    *) esc_rc_path "$1" ;;
  esac
}

strip_rc_block() {
  _src=$1
  _dst=$2
  awk '
    BEGIN { skip=0; held_blank=0 }
    /^# >>> tapwing installer >>>$/ { skip=1; held_blank=0; next }
    /^# <<< tapwing installer <<<$/ { skip=0; next }
    skip { next }
    /^$/ {
      if (held_blank) print ""
      held_blank=1
      next
    }
    {
      if (held_blank) { print ""; held_blank=0 }
      print
    }
    END {
      if (held_blank) print ""
    }
  ' "$_src" >"$_dst"
}

add_path_to_shell_rc() {
  detect_shell_rc
  [ -n "$shell_rc" ] || return 1
  _rc_target=$(resolve_rc_path "$shell_rc") || return 1
  _rc_dir=$(dirname "$_rc_target")
  mkdir -p "$_rc_dir" 2>/dev/null || return 1
  [ -w "$_rc_dir" ] || return 1
  if [ -e "$_rc_target" ] && [ ! -w "$_rc_target" ]; then
    return 1
  fi

  _rc_tmp="$_rc_target.tapwing.tmp.$$"
  if [ -f "$_rc_target" ]; then
    strip_rc_block "$_rc_target" "$_rc_tmp" || { rm -f "$_rc_tmp"; return 1; }
  else
    : >"$_rc_tmp" || { rm -f "$_rc_tmp"; return 1; }
  fi

  if [ -s "$_rc_tmp" ] && [ -n "$(tail -n 1 "$_rc_tmp")" ]; then
    printf '\n' >>"$_rc_tmp"
  fi

  _rc_bin=$(format_rc_path "$bin_dir")
  if [ "$shell_kind" = "fish" ]; then
    printf '# >>> tapwing installer >>>\nfish_add_path "%s"\n# <<< tapwing installer <<<\n' "$_rc_bin" >>"$_rc_tmp"
  else
    printf '# >>> tapwing installer >>>\nexport PATH="%s:$PATH"\n# <<< tapwing installer <<<\n' "$_rc_bin" >>"$_rc_tmp"
  fi
  mv "$_rc_tmp" "$_rc_target" || { rm -f "$_rc_tmp"; return 1; }
  updated_rc="$shell_rc"
  return 0
}

remove_path_from_shell_rc() {
  detect_shell_rc
  [ -n "$shell_rc" ] || return 0
  _rc_target=$(resolve_rc_path "$shell_rc") || return 0
  [ -f "$_rc_target" ] && [ -w "$_rc_target" ] || return 0
  grep -qx '# >>> tapwing installer >>>' "$_rc_target" 2>/dev/null || return 0
  _rc_tmp="$_rc_target.tapwing.tmp.$$"
  if strip_rc_block "$_rc_target" "$_rc_tmp" && mv "$_rc_tmp" "$_rc_target"; then
    note "removed Tapwing PATH block from $shell_rc"
  else
    rm -f "$_rc_tmp"
  fi
}

while [ $# -gt 0 ]; do
  case "$1" in
    --prefix) [ $# -ge 2 ] || die "--prefix needs a value"; prefix=$2; shift 2 ;;
    --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; bin_dir=$2; shift 2 ;;
    --sha256) [ $# -ge 2 ] || die "--sha256 needs a value"; expected_sha=$2; shift 2 ;;
    --version) [ $# -ge 2 ] || die "--version needs a value"; want_version=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || die "--repo needs a value"; repo=$2; shift 2 ;;
    --target) [ $# -ge 2 ] || die "--target needs a value"; want_target=$2; shift 2 ;;
    --no-modify-path) no_modify_path=1; shift ;;
    --print-target) print_target=1; shift ;;
    --uninstall) uninstall=1; shift ;;
    -h|--help) usage; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) [ -z "$source" ] || die "only one SOURCE or VERSION is allowed"; source=$1; shift ;;
  esac
done

detect_host_targets

if [ "$print_target" = 1 ]; then
  [ -n "$default_target" ] || die "unsupported platform: $(uname -s) $(uname -m)"
  printf '%s\n' "$default_target"
  exit 0
fi

if [ -n "$want_version" ] && [ -n "$source" ]; then
  die "pass either SOURCE or --version, not both"
fi

# Shorthand: `curl ... | bash -s 0.1.0` (or `v0.1.0` / `latest`) when no local path matches.
if [ -z "$want_version" ] && [ -n "$source" ]; then
  case "$source" in
    http://*|https://*) ;;
    *)
      if [ ! -e "$source" ] && is_version_spec "$source"; then
        want_version=$source
        source=""
      fi
      ;;
  esac
fi

if [ -z "$want_version" ] && [ -z "$source" ] && [ -n "$env_version" ]; then
  want_version=$env_version
fi

mkdir -p "$prefix" "$bin_dir"
prefix=$(cd "$prefix" && pwd -P)
bin_dir=$(cd "$bin_dir" && pwd -P)
link="$bin_dir/tapwing"

# True when <bin dir>/tapwing is a symlink into this prefix (ours to replace or remove).
link_is_ours() {
  [ -L "$link" ] || return 1
  case "$(readlink "$link")" in "$prefix"/*) return 0 ;; *) return 1 ;; esac
}

if [ "$uninstall" = 1 ]; then
  if link_is_ours; then rm -f "$link"; elif [ -e "$link" ] || [ -L "$link" ]; then note "leaving $link (not installed by this script)"; fi
  rm -rf "$prefix/versions" "$prefix/current"
  if [ "$no_modify_path" != 1 ]; then
    remove_path_from_shell_rc
  fi
  note "removed Tapwing from $prefix"
  note "driver caches remain: rm -rf \"$HOME/.tapwing/native-driver\" \"$HOME/.tapwing/ios-native-driver\""
  exit 0
fi

# No SOURCE and no --version: the unpacked release this script ships in, else
# the latest GitHub Release. Piped into bash/sh, $0 is the shell ("sh", "bash", ...),
# which never sits next to a share/tapwing/release.
if [ -z "$source" ] && [ -z "$want_version" ]; then
  if [ -f "$0" ] && [ -f "$(dirname "$0")/share/tapwing/release" ]; then
    : # installs the unpacked release below
  else
    want_version=latest
  fi
fi

# Prints the tag of the latest GitHub Release of $repo. github.com/<repo>/releases/latest
# redirects to …/releases/tag/<tag>, or to …/releases when nothing is published yet.
resolve_latest_tag() {
  latest_url="https://github.com/$repo/releases/latest"
  require_downloader
  if [ "$downloader" = "curl" ]; then
    effective=$(curl -fsSLI --retry 3 -o /dev/null -w '%{url_effective}' "$latest_url") \
      || die "could not reach $latest_url; pass --version <VER> or a tarball"
  else
    headers=$(wget --tries=3 --max-redirect=10 --spider -S "$latest_url" 2>&1) \
      || die "could not reach $latest_url; pass --version <VER> or a tarball"
    effective=$(printf '%s\n' "$headers" | sed -n 's/^[[:space:]]*[Ll]ocation:[[:space:]]*//p' | tr -d '\r' | tail -n 1)
    [ -n "$effective" ] || effective="$latest_url"
  fi
  case "$effective" in
    */releases/tag/*) tag=${effective##*/releases/tag/} ;;
    *) die "$repo has no published GitHub Release yet ($latest_url led to $effective); pass --version <VER> or build one with scripts/dist.sh" ;;
  esac
  tag=${tag%%\?*}
  tag=${tag%%#*}
  tag=${tag%/}
  case "$tag" in
    ""|latest|v|*[!A-Za-z0-9._+-]*) die "unexpected latest release tag '$tag' (from $effective)" ;;
    v*) ;;
    *) die "latest release tag '$tag' does not start with v (from $effective)" ;;
  esac
  printf '%s\n' "$tag"
}

if [ -n "$want_version" ]; then
  if [ "$rosetta_arm64" = 1 ] && [ -z "$want_target" ]; then
    note "Apple Silicon detected (Rosetta shell); installing native $default_target build"
  fi
  if [ "$want_version" = latest ]; then
    want_version=$(resolve_latest_tag)
    note "latest release of $repo is $want_version"
  fi
  clean_version=${want_version#v}
  [ -n "$clean_version" ] || die "--version cannot be empty"
  dl_target=${want_target:-$default_target}
  [ -n "$dl_target" ] || die "unsupported platform: $(uname -s) $(uname -m) (pass --target or a release URL)"
  source="https://github.com/$repo/releases/download/v$clean_version/tapwing-$clean_version-$dl_target.tar.gz"
fi

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    die "sha256sum or shasum is required to verify checksums"
  fi
}

# verify <tarball> <checksum file or empty>
verify() {
  want=$expected_sha
  if [ -z "$want" ] && [ -n "$2" ] && [ -f "$2" ]; then
    want=$(cut -d' ' -f1 <"$2")
    [ -n "$want" ] || die "$(basename "$2") has no checksum"
  fi
  if [ -z "$want" ]; then
    note "warning: no checksum for $(basename "$1"); pass --sha256 to verify it"
    return 0
  fi
  got=$(sha256 "$1")
  [ "$got" = "$want" ] || die "$(basename "$1"): SHA-256 $got, expected $want"
}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/tapwing-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

case "$source" in
  "")
    release=$(cd "$(dirname "$0")" && pwd -P)
    [ -f "$release/share/tapwing/release" ] || die "not inside an unpacked release; pass a tarball, URL, directory, or --version"
    ;;
  http://*|https://*)
    require_downloader
    tarball="$tmp/$(basename "$source")"
    note "downloading $source"
    download_file "$source" "$tarball" 0
    sums=""
    if [ -z "$expected_sha" ] && download_file "$source.sha256" "$tarball.sha256" 1; then sums="$tarball.sha256"; fi
    # A GitHub Release always publishes the .sha256: never install one unverified.
    if [ -n "$want_version" ] && [ -z "$expected_sha" ] && [ -z "$sums" ]; then
      die "could not download $source.sha256; pass --sha256 to install without it"
    fi
    verify "$tarball" "$sums"
    ;;
  *)
    if [ -d "$source" ]; then
      release=$(cd "$source" && pwd -P)
    elif [ -f "$source" ]; then
      tarball=$source
      verify "$tarball" "$source.sha256"
    else
      die "$source does not exist"
    fi
    ;;
esac

if [ -n "${tarball:-}" ]; then
  mkdir "$tmp/unpacked"
  tar -xzf "$tarball" -C "$tmp/unpacked"
  set -- "$tmp"/unpacked/tapwing-*
  [ $# -eq 1 ] && [ -d "$1" ] || die "$(basename "$tarball") is not a Tapwing release tarball"
  release=$1
fi

info="$release/share/tapwing/release"
[ -f "$info" ] && [ -x "$release/bin/tapwing" ] || die "$release is not a Tapwing release (no bin/tapwing or share/tapwing/release)"
version=$(sed -n 's/^version=//p' "$info")
target=$(sed -n 's/^target=//p' "$info")
[ -n "$version" ] && [ -n "$target" ] || die "$info has no version or target"

case " $host_targets " in
  *" $target "*) ;;
  *) die "this release is for $target, not for $(uname -s) $(uname -m)" ;;
esac

if [ -e "$link" ] || [ -L "$link" ]; then
  link_is_ours || die "$link exists and was not installed by this script; remove it or pass --bin-dir"
fi

# Copy next to the final place, then swap it in, so a failed copy never leaves a half install.
mkdir -p "$prefix/versions"
staging="$prefix/versions/.$version.tmp.$$"
rm -rf "$staging"
cp -R "$release" "$staging"
rm -rf "$prefix/versions/$version"
mv "$staging" "$prefix/versions/$version"
ln -sfn "versions/$version" "$prefix/current"
ln -sfn "$prefix/current/bin/tapwing" "$link"

for old in "$prefix"/versions/*; do
  [ -d "$old" ] && [ "$(basename "$old")" != "$version" ] || continue
  rm -rf "$old"
  note "removed old version $(basename "$old")"
done

# Through the symlink, like a user would run it: proves the bundle is found and intact.
"$link" --version || die "the installed tapwing failed its self-check (see above)"
note "installed Tapwing $version ($target) into $prefix/versions/$version"

path_has_dir() {
  _want=$1
  case ":$PATH:" in
    *":$_want:"*) return 0 ;;
  esac
  _rest=$PATH
  while [ -n "$_rest" ]; do
    case "$_rest" in
      *:*) _entry=${_rest%%:*}; _rest=${_rest#*:} ;;
      *) _entry=$_rest; _rest="" ;;
    esac
    [ -n "$_entry" ] && [ -d "$_entry" ] || continue
    _real=$(cd "$_entry" 2>/dev/null && pwd -P) || continue
    if [ "$_real" = "$_want" ]; then
      return 0
    fi
  done
  return 1
}

updated_rc=""
if path_has_dir "$bin_dir"; then
  note "run 'tapwing --version' to get started"
else
  if [ "$no_modify_path" != 1 ] && add_path_to_shell_rc; then
    note "added $bin_dir to PATH in $updated_rc"
    if [ "$shell_kind" = "fish" ]; then
      note "restart your terminal (or run 'source \"$updated_rc\"'), then run 'tapwing --version'"
    else
      note "restart your terminal (or run '. \"$updated_rc\"'), then run 'tapwing --version'"
    fi
  else
    detect_shell_rc
    if [ "$shell_kind" = "fish" ]; then
      note "add $bin_dir to PATH, e.g. fish_add_path \"$bin_dir\""
    else
      note "add $bin_dir to PATH, e.g. echo 'export PATH=\"$bin_dir:\$PATH\"' >> $shell_rc"
    fi
  fi
fi
note "uninstall: $prefix/current/install.sh --uninstall --prefix \"$prefix\" --bin-dir \"$bin_dir\""
