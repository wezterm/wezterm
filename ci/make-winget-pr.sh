#!/usr/bin/env bash
# Create a winget-pkgs branch for a WezTerm Windows installer and push it.
#
# Usage:
#   bash ci/make-winget-pr.sh <winget_repo_path> <setup_exe>
#   bash ci/make-winget-pr.sh <winget_repo_path> <setup_exe> nightly
#
# <winget_repo_path> is a checkout of the winget-pkgs fork. The script hashes
# <setup_exe>, writes the manifest, commits, and pushes a branch.
#
# For a stable build the caller opens the pull request.
#
# Pass nightly to publish wez.wezterm.nightly against the rolling nightly
# release. The nightly installer lives at a fixed URL that every upload
# replaces, so this runs after each upload and manages the pull request
# itself (GITHUB_TOKEN must be set): it reuses a single fork branch, so at
# most one PR is open at a time, and it does nothing when winget-pkgs, our
# open PR, or another open wez.wezterm.nightly PR (such as wingetbot's)
# already carries the current hash.
set -xeo pipefail

winget_repo=$1
setup_exe=$2
channel=${3:-stable}

case "$channel" in
  stable|nightly) ;;
  *)
    echo "unknown channel '$channel': expected 'stable' or 'nightly'" >&2
    exit 1
    ;;
esac

if [ ! -f "$setup_exe" ]; then
  echo "installer '$setup_exe' not found" >&2
  exit 1
fi

# Derive everything that depends on the wezterm checkout or the installer
# before we cd into the winget repo.
VERSION=$(ci/tag-name.sh)
release_date=$(git show -s "--format=%cd" "--date=format:%Y-%m-%d")
exehash=$(sha256sum -b "$setup_exe" | cut -f1 -d' ' | tr a-f A-F)
setup_exe_name=$(basename "$setup_exe")

if [ "$channel" = "nightly" ]; then
  branch="wezterm-nightly"
  package_id="wez.wezterm.nightly"
  package_dir="manifests/w/wez/wezterm/nightly"
  installer_url="https://github.com/wezterm/wezterm/releases/download/nightly/${setup_exe_name}"
  package_name="WezTerm (Nightly)"
  short_description="A GPU-accelerated cross-platform terminal emulator and multiplexer implemented in Rust (nightly build)"
  release_notes_url="https://github.com/wezterm/wezterm/releases/tag/nightly"
else
  branch="$VERSION"
  package_id="wez.wezterm"
  package_dir="manifests/w/wez/wezterm"
  installer_url="https://github.com/wezterm/wezterm/releases/download/${VERSION}/${setup_exe_name}"
  package_name="WezTerm"
  short_description="A GPU-accelerated cross-platform terminal emulator and multiplexer implemented in Rust"
  release_notes_url="https://wezterm.org/changelog.html#${VERSION}"
fi
manifest_dir="${package_dir}/${VERSION}"
commit_message="New version: ${package_id} version ${VERSION}"

cd "$winget_repo" || exit 1

# First sync repo with upstream
git remote add upstream https://github.com/microsoft/winget-pkgs.git || true
git fetch upstream master --quiet

if [ "$channel" = "nightly" ]; then
  upstream_repo="microsoft/winget-pkgs"
  fork_owner=$(git remote get-url origin | sed -E 's#^.*github\.com[:/]([^/]+)/.*$#\1#')
  open_pr=$(gh pr list -R "$upstream_repo" --state open --head "$branch" \
    --json number,headRepositoryOwner \
    --jq ".[] | select(.headRepositoryOwner.login == \"${fork_owner}\") | .number" | head -n1)

  if git grep -q -i "InstallerSha256: ${exehash}" upstream/master -- "$package_dir"; then
    echo "winget-pkgs already has the current nightly hash; nothing to do"
    if [ -n "$open_pr" ]; then
      gh pr close "$open_pr" -R "$upstream_repo" --delete-branch \
        --comment "Superseded: winget-pkgs already has the current nightly installer hash."
    fi
    exit 0
  fi

  # wingetbot and community contributors also open wez.wezterm.nightly PRs
  # from their own forks. If one of them already carries the current hash,
  # leave the update to it rather than racing it with a duplicate.
  other_prs=$(gh pr list -R "$upstream_repo" --state open \
    --search "${package_id} in:title" \
    --json number,headRepositoryOwner,headRefName \
    --jq ".[] | select((.headRepositoryOwner.login + \":\" + .headRefName) != \"${fork_owner}:${branch}\") | .number")
  for pr in $other_prs; do
    if gh pr diff "$pr" -R "$upstream_repo" | grep -q -i "^+.*InstallerSha256: ${exehash}"; then
      echo "open PR #${pr} already has the current nightly hash; nothing to do"
      if [ -n "$open_pr" ]; then
        gh pr close "$open_pr" -R "$upstream_repo" --delete-branch \
          --comment "Superseded by #${pr}, which already has the current nightly installer hash."
      fi
      exit 0
    fi
  done

  if [ -n "$open_pr" ] && git fetch origin "$branch" --quiet \
    && git grep -q -i "InstallerSha256: ${exehash}" FETCH_HEAD -- "$package_dir"; then
    echo "open PR #${open_pr} already has the current nightly hash; nothing to do"
    exit 0
  fi
fi

git checkout -B "$branch" upstream/master

if [ "$channel" = "nightly" ]; then
  # The nightly installer URL is fixed and its contents are replaced by every
  # build, so any previously published nightly version now has a stale hash.
  # Replace it rather than accumulating broken versions.
  for old in "$package_dir"/*/; do
    [ -f "${old}${package_id}.yaml" ] || continue
    git rm -r --quiet "$old"
  done
fi

# Create the directory structure
mkdir -p "$manifest_dir"

cat > "$manifest_dir/${package_id}.installer.yaml" <<-EOT
PackageIdentifier: ${package_id}
PackageVersion: $VERSION
MinimumOSVersion: 10.0.17763.0
InstallerType: inno
UpgradeBehavior: install
ReleaseDate: $release_date
Installers:
- Architecture: x64
  InstallerUrl: ${installer_url}
  InstallerSha256: $exehash
  ProductCode: '{BCF6F0DA-5B9A-408D-8562-F680AE6E1EAF}_is1'
ManifestType: installer
ManifestVersion: 1.1.0
EOT

cat > "$manifest_dir/${package_id}.locale.en-US.yaml" <<-EOT
PackageIdentifier: ${package_id}
PackageVersion: $VERSION
PackageLocale: en-US
Publisher: Wez Furlong
PublisherUrl: https://wezfurlong.org/
PublisherSupportUrl: https://github.com/wezterm/wezterm/issues
Author: Wez Furlong
PackageName: ${package_name}
PackageUrl: http://wezterm.org
License: MIT
LicenseUrl: https://github.com/wezterm/wezterm/blob/main/LICENSE.md
ShortDescription: ${short_description}
ReleaseNotesUrl: ${release_notes_url}
ManifestType: defaultLocale
ManifestVersion: 1.1.0
EOT

cat > "$manifest_dir/${package_id}.yaml" <<-EOT
PackageIdentifier: ${package_id}
PackageVersion: $VERSION
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.1.0
EOT

git add --all
git diff --cached
git commit -m "$commit_message"

if [ "$channel" = "nightly" ]; then
  # The branch is reused across builds, so replace whatever it held before.
  git push --force --set-upstream origin "$branch" --quiet
  if [ -n "$open_pr" ]; then
    gh pr edit "$open_pr" -R "$upstream_repo" --title "$commit_message"
  else
    gh pr create -R "$upstream_repo" --base master --head "${fork_owner}:${branch}" \
      --title "$commit_message" \
      --body "PR automatically created by release automation in the wezterm repo"
  fi
else
  git push --set-upstream origin "$branch" --quiet
fi
