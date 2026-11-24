#!/usr/bin/env bash
# Cuts a Tapwing release in two steps, because `main` only takes pull requests.
#
#   scripts/release.sh prepare <VERSION> [--dry-run] [--no-push]
#   scripts/release.sh tag     <VERSION> [--dry-run] [--no-push]
#
# VERSION is semver with an optional leading v (0.2.0, v0.2.0, 0.2.0-rc.1).
#
# Both steps start from a clean `main` (no untracked files either) that equals
# origin/main, and refuse a tag v<VERSION> that already exists locally or on origin.
#
# prepare  creates the branch release/v<VERSION> and bumps, in lockstep:
#            crates/tapwing/Cargo.toml             version = "<VERSION>"
#            packages/tapwing_driver/pubspec.yaml  version: <VERSION>
#            crates/tapwing/Cargo.lock             the tapwing entry only (cargo update -p tapwing --offline)
#            bench/parse/Cargo.lock                the same entry: CI checks that crate with --locked
#            packages/tapwing_driver/CHANGELOG.md  "## Unreleased" becomes "## <VERSION>"
#            packages/tapwing_driver/README.md     the `ref: v<VERSION>` of the install snippet
#          verifies with
#            (cd crates/tapwing && cargo test --locked -p tapwing --test init)
#            cargo check --manifest-path bench/parse/Cargo.toml --locked
#            scripts/test-dist.sh
#          commits "release: v<VERSION>", pushes the branch and opens its pull request
#          (with `gh`, when installed), then returns to main. If a check fails, the
#          edited files are restored and the branch is removed.
#
# tag      after that pull request is merged and main is pulled: checks that the
#          version files say VERSION and that the `CI Gate` check passed on the
#          commit (needs `gh`), creates the annotated tag v<VERSION> and pushes it.
#          The pushed tag runs .github/workflows/release.yml, which publishes the
#          GitHub Release and updates the Homebrew tap.
#
# --dry-run  run the checks and print the planned changes and commands; nothing is
#            modified, committed, tagged or pushed (an unreachable origin only warns)
# --no-push  prepare: commit on the branch without pushing or opening a pull request
#            tag:     create the tag locally; push later with: git push origin v<VERSION>
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
CARGO_TOML=crates/tapwing/Cargo.toml
CARGO_LOCK=crates/tapwing/Cargo.lock
PUBSPEC=packages/tapwing_driver/pubspec.yaml
CHANGELOG=packages/tapwing_driver/CHANGELOG.md
DRIVER_README=packages/tapwing_driver/README.md
BENCH_TOML=bench/parse/Cargo.toml
BENCH_LOCK=bench/parse/Cargo.lock
FILES=("$CARGO_TOML" "$CARGO_LOCK" "$BENCH_LOCK" "$PUBSPEC" "$CHANGELOG" "$DRIVER_README")
CI_CHECK="CI Gate"
USAGE="usage: scripts/release.sh prepare|tag <VERSION> [--dry-run] [--no-push]"

die() { echo "release: $*" >&2; exit 1; }
note() { echo "release: $*" >&2; }

mode=""
version=""
dry_run=false
push=true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) dry_run=true; shift ;;
    --no-push) push=false; shift ;;
    -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
    -*) die "unknown option: $1" ;;
    prepare|tag)
      if [[ -z "$mode" ]]; then mode=$1; else die "only one of prepare or tag is allowed"; fi
      shift ;;
    *)
      [[ -n "$mode" ]] || die "$USAGE"
      [[ -z "$version" ]] || die "only one VERSION is allowed"
      version=$1; shift ;;
  esac
done

version=${version#v}
[[ -n "$mode" && -n "$version" ]] || die "$USAGE"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "$version is not a semver version (e.g. 0.2.0 or 0.2.0-rc.1)"
tag="v$version"
release_branch="release/$tag"

cd "$ROOT"
command -v git >/dev/null || die "git is required"

# --- Preconditions -----------------------------------------------------------------
branch=$(git symbolic-ref --quiet --short HEAD || true)
[[ "$branch" == "main" ]] || die "releases are cut from main, not ${branch:-a detached HEAD}"
[[ -z "$(git status --porcelain --untracked-files=all)" ]] \
  || die "the working tree is not clean (commit, stash or remove these first):"$'\n'"$(git status --short --untracked-files=all)"
if git rev-parse --quiet --verify "refs/tags/$tag" >/dev/null; then
  die "tag $tag already exists locally"
fi

if git fetch --quiet origin main; then
  if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
    die "tag $tag already exists on origin"
  fi
  behind=$(git rev-list --count HEAD..origin/main)
  [[ "$behind" == 0 ]] || die "main is $behind commit(s) behind origin/main; pull first"
  ahead=$(git rev-list --count origin/main..HEAD)
  [[ "$ahead" == 0 ]] || die "main is $ahead commit(s) ahead of origin/main; main only takes pull requests, so open one for them first"
elif $dry_run; then
  note "warning: could not fetch origin main; skipped the origin tag and up-to-date checks"
else
  die "could not fetch origin main (needed to check the tag and that main is up to date)"
fi

current=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$CARGO_TOML" | head -n1)
current_driver=$(sed -n 's/^version: *//p' "$PUBSPEC" | head -n1)
[[ -n "$current" ]] || die "no version in $CARGO_TOML"
[[ -n "$current_driver" ]] || die "no version in $PUBSPEC"

# bump_toml <file> <version>: the version of the [package] section.
bump_toml() {
  awk -v v="$2" '
    /^\[/ { section = $0 }
    !done && section == "[package]" && /^version = "/ { print "version = \"" v "\""; done = 1; next }
    { print }
  ' "$1"
}
# bump_pubspec <file> <version>: the top-level version.
bump_pubspec() {
  awk -v v="$2" '!done && /^version:/ { print "version: " v; done = 1; next } { print }' "$1"
}
# bump_lock <file> <version>: the version line of the tapwing package (preview only).
bump_lock() {
  awk -v v="$2" '
    /^name = "tapwing"$/ { pkg = 1; print; next }
    pkg && /^version = "/ { print "version = \"" v "\""; pkg = 0; next }
    /^\[\[package\]\]/ { pkg = 0 }
    { print }
  ' "$1"
}
# bump_changelog <file> <version>: the first "## Unreleased" heading becomes "## <version>".
bump_changelog() {
  awk -v v="$2" '!done && /^## Unreleased[[:space:]]*$/ { print "## " v; done = 1; next } { print }' "$1"
}
# bump_readme_ref <file> <version>: the git `ref: v…` of the install snippet.
bump_readme_ref() {
  awk -v v="$2" '
    !done && /^[[:space:]]*ref: v[0-9]/ { sub(/ref: v.*$/, "ref: v" v); done = 1 }
    { print }
  ' "$1"
}
# has_changelog_entry <file> <version>
has_changelog_entry() {
  grep -qxF "## $2" "$1"
}

WORK=$(mktemp -d "${TMPDIR:-/tmp}/tapwing-release.XXXXXX")
on_release_branch=false
committed=false
cleanup() {
  local rc=$?
  rm -rf "$WORK"
  if $on_release_branch && ! $committed; then
    git checkout --quiet -- "${FILES[@]}" 2>/dev/null || true
    git switch --quiet main 2>/dev/null || true
    git branch --quiet -D "$release_branch" 2>/dev/null || true
    note "restored ${FILES[*]} and removed $release_branch"
  fi
  exit "$rc"
}
trap cleanup EXIT

bump_toml "$CARGO_TOML" "$version" >"$WORK/Cargo.toml"
bump_pubspec "$PUBSPEC" "$version" >"$WORK/pubspec.yaml"
bump_lock "$CARGO_LOCK" "$version" >"$WORK/Cargo.lock"
bump_lock "$BENCH_LOCK" "$version" >"$WORK/bench-Cargo.lock"
bump_changelog "$CHANGELOG" "$version" >"$WORK/CHANGELOG.md"
bump_readme_ref "$DRIVER_README" "$version" >"$WORK/README.md"
grep -qx "version = \"$version\"" "$WORK/Cargo.toml" || die "could not set the version in $CARGO_TOML"
grep -qx "version: $version" "$WORK/pubspec.yaml" || die "could not set the version in $PUBSPEC"

needs_bump=false
if [[ "$current" != "$version" || "$current_driver" != "$version" ]] \
  || ! cmp -s "$WORK/Cargo.lock" "$CARGO_LOCK" \
  || ! cmp -s "$WORK/bench-Cargo.lock" "$BENCH_LOCK" \
  || ! cmp -s "$WORK/CHANGELOG.md" "$CHANGELOG" \
  || ! cmp -s "$WORK/README.md" "$DRIVER_README"; then
  needs_bump=true
fi

# --- tag -------------------------------------------------------------------------------
if [[ "$mode" == tag ]]; then
  if $needs_bump || ! has_changelog_entry "$CHANGELOG" "$version"; then
    die "main is not prepared for $tag (CLI $current, driver $current_driver, or $CHANGELOG has no \"## $version\"); run: scripts/release.sh prepare $version, merge its pull request and pull main"
  fi
  sha=$(git rev-parse HEAD)
  note "releasing $tag at $(git rev-parse --short HEAD)"

  # The release workflow builds whatever the tag points at and runs no tests itself.
  if command -v gh >/dev/null; then
    state=$(gh api "repos/{owner}/{repo}/commits/$sha/check-runs" \
      --jq "[.check_runs[] | select(.name == \"$CI_CHECK\")] | sort_by(.started_at) | last | (.conclusion // .status // \"missing\")" 2>/dev/null || true)
    state=${state:-unknown}
    if [[ "$state" != success ]]; then
      msg="the \"$CI_CHECK\" check on $sha is \"$state\", not \"success\""
      if $dry_run; then note "warning: $msg"; else die "$msg; wait for it to pass on main"; fi
    else
      note "\"$CI_CHECK\" passed on $(git rev-parse --short HEAD)"
    fi
  elif $dry_run; then
    note "warning: gh is not installed; skipped the \"$CI_CHECK\" check"
  else
    die "gh is required to check that \"$CI_CHECK\" passed on $sha"
  fi

  plan=("git tag -a $tag -m \"Release $tag\"")
  if $push; then plan+=("git push origin $tag"); fi
  if $dry_run; then
    note "dry run; would run:"
    for cmd in "${plan[@]}"; do echo "  $cmd"; done
    note "dry run: nothing was tagged or pushed"
    exit 0
  fi

  git tag -a "$tag" -m "Release $tag"
  note "tagged $tag at $(git rev-parse --short HEAD)"
  if $push; then
    git push origin "$tag"
    note "pushed $tag; the Release workflow now builds and publishes it"
  else
    note "not pushed; publish with: git push origin $tag"
  fi
  exit 0
fi

# --- prepare ---------------------------------------------------------------------------
command -v cargo >/dev/null || die "cargo is required"
if ! $needs_bump; then
  has_changelog_entry "$CHANGELOG" "$version" || die "$CHANGELOG has no \"## Unreleased\" or \"## $version\" section; describe the release there first"
  note "main already says $version; nothing to prepare. Next: scripts/release.sh tag $version"
  exit 0
fi
has_changelog_entry "$WORK/CHANGELOG.md" "$version" \
  || die "$CHANGELOG has no \"## Unreleased\" or \"## $version\" section; describe the release there first"
if git rev-parse --quiet --verify "refs/heads/$release_branch" >/dev/null; then
  die "branch $release_branch already exists locally (delete it to start over: git branch -D $release_branch)"
fi
if git ls-remote --exit-code --heads origin "refs/heads/$release_branch" >/dev/null 2>&1; then
  die "branch $release_branch already exists on origin; merge or close its pull request first"
fi

note "preparing $tag (CLI $current, driver $current_driver)"
note "planned changes:"
diff -u --label "a/$CARGO_TOML" --label "b/$CARGO_TOML" "$CARGO_TOML" "$WORK/Cargo.toml" || true
diff -u --label "a/$PUBSPEC" --label "b/$PUBSPEC" "$PUBSPEC" "$WORK/pubspec.yaml" || true
diff -u --label "a/$CARGO_LOCK" --label "b/$CARGO_LOCK" "$CARGO_LOCK" "$WORK/Cargo.lock" || true
diff -u --label "a/$BENCH_LOCK" --label "b/$BENCH_LOCK" "$BENCH_LOCK" "$WORK/bench-Cargo.lock" || true
diff -u --label "a/$CHANGELOG" --label "b/$CHANGELOG" "$CHANGELOG" "$WORK/CHANGELOG.md" || true
diff -u --label "a/$DRIVER_README" --label "b/$DRIVER_README" "$DRIVER_README" "$WORK/README.md" || true

have_gh=false
if command -v gh >/dev/null; then have_gh=true; fi
pr_title="release: $tag"
pr_body="Bumps the CLI and \`tapwing_driver\` to $version. After this is merged: \`git switch main && git pull && scripts/release.sh tag $version\`."

plan=(
  "git switch -c $release_branch"
  "cargo update --manifest-path $CARGO_TOML -p tapwing --offline"
  "cargo update --manifest-path $BENCH_TOML -p tapwing --offline"
  "(cd crates/tapwing && cargo test --locked -p tapwing --test init)"
  "cargo check --manifest-path $BENCH_TOML --locked"
  "scripts/test-dist.sh"
  "git commit -m \"release: $tag\" ${FILES[*]}"
)
if $push; then
  plan+=("git push -u origin $release_branch")
  if $have_gh; then plan+=("gh pr create --base main --head $release_branch --title \"$pr_title\""); fi
fi
plan+=("git switch main")

if $dry_run; then
  note "dry run; would run:"
  for cmd in "${plan[@]}"; do echo "  $cmd"; done
  note "dry run: nothing was modified, committed or pushed"
  exit 0
fi

git switch --quiet -c "$release_branch"
on_release_branch=true
cp "$WORK/Cargo.toml" "$CARGO_TOML"
cp "$WORK/pubspec.yaml" "$PUBSPEC"
cp "$WORK/CHANGELOG.md" "$CHANGELOG"
cp "$WORK/README.md" "$DRIVER_README"
cargo update --manifest-path "$CARGO_TOML" -p tapwing --offline
cargo update --manifest-path "$BENCH_TOML" -p tapwing --offline

note "verifying: cargo test --locked -p tapwing --test init"
(cd crates/tapwing && cargo test --locked -p tapwing --test init)
note "verifying: cargo check --manifest-path $BENCH_TOML --locked"
cargo check --manifest-path "$BENCH_TOML" --locked
note "verifying: scripts/test-dist.sh"
scripts/test-dist.sh

# Only the version files may have changed.
unexpected=$(git status --porcelain --untracked-files=all | awk '{ print $NF }' \
  | grep -vxF -e "$CARGO_TOML" -e "$CARGO_LOCK" -e "$BENCH_LOCK" -e "$PUBSPEC" -e "$CHANGELOG" -e "$DRIVER_README" || true)
[[ -z "$unexpected" ]] || die "verification left unexpected changes: $unexpected"

git add -- "${FILES[@]}"
git commit --quiet -m "release: $tag" -- "${FILES[@]}"
committed=true
note "committed release: $tag on $release_branch"

if $push; then
  git push --quiet -u origin "$release_branch"
  if $have_gh; then
    gh pr create --base main --head "$release_branch" --title "$pr_title" --body "$pr_body"
  else
    note "gh is not installed; open a pull request for $release_branch into main"
  fi
  git switch --quiet main
  note "next, once the pull request is merged: git pull && scripts/release.sh tag $version"
else
  git switch --quiet main
  note "not pushed; continue with: git push -u origin $release_branch, open its pull request, merge it, then scripts/release.sh tag $version"
fi
