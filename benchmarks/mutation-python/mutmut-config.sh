#!/usr/bin/env bash
# Appends mutmut's configuration for one corpus project to its pyproject:
# where its source is, and what its tests need copied beside mutmut's
# mutated tree. Nothing else differs from mutmut's defaults.
#   mutmut-config.sh <project> <src dir>
set -euo pipefail
project=$1
cd "$2"
grep -q '^\[tool.mutmut\]' pyproject.toml && exit 0
case "$project" in
  python-slugify) printf '\n[tool.mutmut]\nsource_paths = ["slugify"]\nalso_copy = ["tools/", "README.md"]\n' ;;
  cachetools) printf '\n[tool.mutmut]\nsource_paths = ["src/cachetools"]\n' ;;
  parse) printf '\n[tool.mutmut]\nsource_paths = ["parse"]\nalso_copy = ["README.rst"]\n' ;;
  humanize) printf '\n[tool.mutmut]\nsource_paths = ["src/humanize"]\nalso_copy = ["scripts/"]\nprocess_isolation = "forkserver"\n' ;;
esac >> pyproject.toml
