#!/usr/bin/env bash
# Clones one corpus project at its pinned tag into <dir>/src and installs it
# with its test dependencies into <dir>/.venv (uv, Python 3.12).
#   setup.sh <project> <dir>
# The projects, their tags and why each is set up so are in README.md.
set -euo pipefail
project=$1
dir=$2
mkdir -p "$dir"
cd "$dir"
case "$project" in
  python-slugify) repo=https://github.com/un33k/python-slugify.git tag=v9.2.0 packages="pytest" ;;
  cachetools) repo=https://github.com/tkem/cachetools.git tag=v8.0.2 packages="pytest" ;;
  parse) repo=https://github.com/r1chardj0n3s/parse.git tag=1.22.4 packages="pytest pytest-cov typing-extensions" ;;
  humanize) repo=https://github.com/python-humanize/humanize.git tag=4.17.0 packages="pytest freezegun pytest-benchmark babel" ;;
  *) echo "unknown project $project" >&2; exit 2 ;;
esac
if [ ! -d src ]; then
  git clone -q --depth 1 --branch "$tag" "$repo" src
fi
uv venv -q --python 3.12 .venv
VIRTUAL_ENV=$PWD/.venv uv pip install -q -e ./src $packages "mutmut==3.8.1"
if [ "$project" = humanize ]; then
  # test_update_translations runs gettext's tools, and skips without them.
  command -v msgfmt > /dev/null || { sudo apt-get update -qq && sudo apt-get install -y -qq gettext; } || true
  # tox's compile-translations: the i18n tests skip without them.
  .venv/bin/pybabel --quiet compile --directory src/src/humanize/locale --domain humanize
fi
