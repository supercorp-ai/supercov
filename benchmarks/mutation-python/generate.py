"""Writes mutmut's mutated tree (`mutants/`) for a project without running
its tests: what `mutmut run` does before it runs anything.

    <venv>/bin/python generate.py <project src dir>
"""

import os
import sys
from pathlib import Path

os.chdir(sys.argv[1])

from mutmut.__main__ import create_mutants  # noqa: E402
from mutmut.mutation.trampoline import set_mutant_under_test  # noqa: E402
from mutmut.utils.file_utils import copy_also_copy_files, copy_src_dir, setup_source_paths  # noqa: E402

set_mutant_under_test("mutant_generation")
Path("mutants").mkdir(exist_ok=True)
copy_src_dir()
copy_also_copy_files()
setup_source_paths()
stats = create_mutants(os.cpu_count() or 4)
print(f"{stats.mutated} files mutated")
