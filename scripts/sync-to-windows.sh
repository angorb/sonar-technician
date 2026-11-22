#!/usr/bin/env bash
# Mirror the working tree to C:\dev\sonar-technician for native Windows builds.
# The copy is disposable: everything there except target/ and probe-*/ is
# replaced on each run. The WSL repo is the only one with git history.
set -euo pipefail

src="$(cd "$(dirname "$0")/.." && pwd)"
dest="${SONAR_TECH_WIN_DIR:-/mnt/c/dev/sonar-technician}"

mkdir -p "$dest"
find "$dest" -mindepth 1 -maxdepth 1 \
  ! -name target ! -name 'probe-*' -exec rm -rf {} +

cd "$src"
# Tracked files plus untracked-but-not-ignored ones (so new files sync before commit).
git ls-files -z --cached --others --exclude-standard \
  | tar --null -T - -cf - \
  | tar -xf - -C "$dest"

echo "synced $(git ls-files --cached --others --exclude-standard | wc -l) files -> $dest"
