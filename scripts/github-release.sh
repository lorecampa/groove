#!/usr/bin/env bash
# Called after release.sh validate and asset checksum verification by publish.yml.
# GitHub operations require --execute; publication additionally requires --publish.
set -euo pipefail

die() { printf '%s\n' "$*" >&2; exit 1; }
if [[ $# -lt 5 || $# -gt 6 || ${5:-} != --execute || (${6:-} != '' && ${6:-} != --publish) ]]; then
  die 'Usage: github-release.sh OWNER/REPO TAG ASSET_DIR NOTES --execute [--publish] (default: draft only)'
fi
repository=$1
release_tag=$2
asset_dir=$(cd -- "$3" && pwd)
notes=$4
[[ $repository =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die 'Invalid repository'
[[ $release_tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[A-Za-z0-9.-]+)?$ ]] || die 'Invalid release tag'
[[ -s $notes ]] || die 'Release notes must not be empty'
scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT
mkdir "$scratch/remote"

# List rather than treating any failed view request as a missing release. Auth,
# network and permission failures must stop before any mutation.
gh api --paginate --slurp "repos/$repository/releases" > "$scratch/releases.json"
jq --arg tag "$release_tag" '[.[][] | select(.tag_name == $tag)] | if length > 1 then error("Duplicate tag") else .[0] end' \
  "$scratch/releases.json" > "$scratch/release.json"
existing=$(jq -r '. != null' "$scratch/release.json")
draft=true
if [[ $existing == true ]]; then
  draft=$(jq -er '.draft | tostring' "$scratch/release.json")
  # Download existing bytes even for immutable published releases. Never delete,
  # replace or clobber an existing asset, including on a draft.
  if [[ $(jq '.assets | length' "$scratch/release.json") -gt 0 ]]; then
    gh release download "$release_tag" --repo "$repository" --dir "$scratch/remote"
  fi
fi

shopt -s nullglob dotglob
local_assets=("$asset_dir"/*)
[[ ${#local_assets[@]} -gt 0 ]] || die 'No release assets'
missing=()
for path in "${local_assets[@]}"; do
  name=${path##*/}
  [[ -f $path && ! -L $path && $name =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "Invalid asset: $name"
  if [[ -f $scratch/remote/$name && ! -L $scratch/remote/$name ]]; then
    cmp -s -- "$path" "$scratch/remote/$name" || die "Existing asset differs: $name; preserve the release and investigate"
  else
    missing+=("$path")
  fi
done
for path in "$scratch/remote"/*; do
  name=${path##*/}
  [[ -f $asset_dir/$name ]] || die "Unexpected existing asset: $name"
done
if [[ $existing == true && $draft != true ]]; then
  [[ ${#missing[@]} -eq 0 ]] || die 'Published release is incomplete; refusing to modify it'
  printf 'Verified published release %s; no changes made.\n' "$release_tag"
  exit 0
fi

if [[ $existing != true ]]; then
  flags=(--draft --verify-tag --title "Groove $release_tag" --notes-file "$notes")
  if [[ $release_tag == *-* ]]; then flags+=(--prerelease); fi
  gh release create "$release_tag" --repo "$repository" "${flags[@]}"
fi
for path in "${missing[@]}"; do
  gh release upload "$release_tag" "$path" --repo "$repository"
done

# Verify the uploaded bytes before publishing. A partial upload remains a draft.
mkdir "$scratch/verified"
gh release download "$release_tag" --repo "$repository" --dir "$scratch/verified"
for path in "${local_assets[@]}"; do
  cmp -s -- "$path" "$scratch/verified/${path##*/}" || die "Uploaded asset failed verification: ${path##*/}"
done
for path in "$scratch/verified"/*; do
  [[ -f $asset_dir/${path##*/} ]] || die "Unexpected uploaded asset: ${path##*/}"
done
gh release edit "$release_tag" --repo "$repository" --notes-file "$notes"
if [[ ${6:-} == --publish ]]; then
  flags=(--draft=false)
  if [[ $release_tag == *-* ]]; then flags+=(--latest=false); else flags+=(--latest); fi
  gh release edit "$release_tag" --repo "$repository" "${flags[@]}"
  printf 'Published verified release %s.\n' "$release_tag"
else
  printf 'Verified draft %s. Repository visibility is unchanged.\n' "$release_tag"
fi
