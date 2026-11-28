#!/usr/bin/env bash
# Appends mutmut's configuration for one corpus project to its pyproject:
# where its source is, and what its tests need copied beside mutmut's
# mutated tree; and parse's pytest-cov off (its configuration turns it on
# for every run), as Supercov's strong pass turns it off. Nothing else differs
# from mutmut's defaults.
#   mutmut-config.sh <project> <src dir>
set -euo pipefail
project=$1
cd "$2"
grep -q '^\[tool.mutmut\]' pyproject.toml && exit 0
case "$project" in
  python-slugify) printf '\n[tool.mutmut]\nsource_paths = ["slugify"]\nalso_copy = ["tools/", "README.md"]\n' ;;
  cachetools) printf '\n[tool.mutmut]\nsource_paths = ["src/cachetools"]\n' ;;
  parse) printf '\n[tool.mutmut]\nsource_paths = ["parse"]\nalso_copy = ["README.rst"]\npytest_add_cli_args = ["--no-cov"]\n' ;;
  humanize) printf '\n[tool.mutmut]\nsource_paths = ["src/humanize"]\nalso_copy = ["scripts/"]\nprocess_isolation = "forkserver"\n' ;;
  markupsafe) printf '\n[tool.mutmut]\nsource_paths = ["src/markupsafe"]\n' ;;
  semver) printf '\n[tool.mutmut]\nsource_paths = ["src/semver"]\nalso_copy = ["docs/", "README.rst", ".pytest.ini"]\npytest_add_cli_args = ["--no-cov"]\nprocess_isolation = "forkserver"\n' ;;
esac >> pyproject.toml
