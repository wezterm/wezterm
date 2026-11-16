#!/usr/bin/env bash
# Create a winget-pkgs branch for a WezTerm Windows installer and push it.
#
# Usage:
#   ./ci/make-winget-pr.sh <winget_repo_path> <setup_exe>
#   ./ci/make-winget-pr.sh <winget_repo_path> <setup_exe> nightly
#
# <winget_repo_path> is a checkout of the winget-pkgs fork. The script hashes
# <setup_exe>, writes the manifest, commits, and pushes a branch. The caller
# opens the pull request. Pass nightly to publish wez.wezterm.nightly against
# the rolling nightly release instead of a tagged stable build.
set -xe

winget_repo=$1
setup_exe=$2
channel=$3
VERSION=$(ci/tag-name.sh)

cd "$winget_repo" || exit 1

# First sync repo with upstream
git remote add upstream https://github.com/microsoft/winget-pkgs.git || true
git fetch upstream master --quiet

if [ "$channel" = "nightly" ]; then
  branch="update-wezterm-nightly-${VERSION}"
  package_id="wez.wezterm.nightly"
  manifest_dir="manifests/w/wez/wezterm/nightly/${VERSION}"
  installer_url="https://github.com/wezterm/wezterm/releases/download/nightly/${setup_exe}"
  package_name="WezTerm (Nightly)"
  short_description="A GPU-accelerated cross-platform terminal emulator and multiplexer implemented in Rust (nightly build)"
  release_notes_url="https://wezterm.org/changelog.html"
  release_date=$(date +%Y-%m-%d)
  commit_message="New version: wez.wezterm.nightly version ${VERSION}"
else
  branch="$VERSION"
  package_id="wez.wezterm"
  manifest_dir="manifests/w/wezterm/wezterm/${VERSION}"
  installer_url="https://github.com/wezterm/wezterm/releases/download/${VERSION}/${setup_exe}"
  package_name="WezTerm"
  short_description="A GPU-accelerated cross-platform terminal emulator and multiplexer implemented in Rust"
  release_notes_url="https://wezterm.org/changelog.html#${VERSION}"
  commit_message="New version: wez.wezterm version ${VERSION}"
fi

git checkout -b "$branch" upstream/master

exehash=$(sha256sum -b ../$setup_exe | cut -f1 -d' ' | tr a-f A-F)

if [ "$channel" != "nightly" ]; then
  release_date=$(git show -s "--format=%cd" "--date=format:%Y-%m-%d")
fi

# Create the directory structure
mkdir "$manifest_dir"

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
git push --set-upstream origin "$branch" --quiet
