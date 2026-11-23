#!/bin/bash
# Diagnosis: runs a command under strace, printing every signal any of its processes sends (other
# than the 0 that only asks whether a process lives) with the sender's pid, as it happens, and each
# process's start, so that the sender can be told apart. Used as SUPERCOV_BENCH_STRONG_WRAP.
command -v strace >/dev/null || sudo apt-get install -y -qq strace >/dev/null 2>&1
exec strace -f -qq -tt -e trace=kill,tgkill,tkill,execve -e signal=none -o >(grep --line-buffered -E 'kill\([^)]*, SIG|execve\(' | grep --line-buffered -v 'execve(.*= -1' | cut -c1-300 >&2) "$@"
