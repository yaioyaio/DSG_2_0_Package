#!/usr/bin/env sh
set -eu
if [ "$#" -ne 2 ]; then
  printf '%s\n' 'Usage: git-completion.sh feature/YYYYMMDD-name STATE_DIR' >&2
  exit 2
fi
feature=$1
state_dir=$2
case "$feature" in
  feature/[0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-*) ;;
  *) printf '%s\n' 'Invalid feature branch.' >&2; exit 2 ;;
esac
project_root=$(git rev-parse --show-toplevel)
cd "$project_root"
tree=$(git rev-parse --verify "refs/heads/$feature^{tree}")
for reference in refs/heads/main refs/heads/develop refs/remotes/origin/main \
  refs/remotes/origin/develop "refs/remotes/origin/$feature"; do
  test "$(git rev-parse --verify "$reference^{tree}")" = "$tree"
done
"${DSG_CLI:-dsg}" --root "$project_root" --state-dir "$state_dir" --json cleanup-git --feature "$feature"
git switch --quiet develop
