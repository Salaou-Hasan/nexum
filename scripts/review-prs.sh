#!/bin/bash
# Review each research pull request before merging it: which files it
# touches, whether its checks are green, and whether it can be merged.
cd /mnt/c/Nexum/nexum
for n in $(gh pr list --limit 40 --json number --jq '.[].number'); do
  title=$(gh pr view "$n" --json title --jq '.title')
  files=$(gh pr view "$n" --json files --jq '.files[].path' | tr '\n' ' ')
  state=$(gh pr view "$n" --json mergeable,mergeStateStatus --jq '.mergeable + "/" + .mergeStateStatus')
  checks=$(gh pr checks "$n" 2>&1 | tail -n +2 | head -3 | awk '{print $2}' | tr '\n' ',')
  echo "#$n  $state  checks=$checks"
  echo "    $title"
  echo "    files: $files"
done
