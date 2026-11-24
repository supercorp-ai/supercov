#!/usr/bin/env bash
# How many fetched artifacts the dependency caches hold: Maven's and Gradle's
# jars and poms, and Gradle's unpacked distributions. What builds write there
# on their own (indexes, locks, a project's own install) is left out, so the
# count grows only when something was fetched.
{
  find ~/.m2/repository -type f \( -name '*.jar' -o -name '*.pom' \) 2> /dev/null
  find ~/.gradle/caches/modules-2/files-2.1 -type f 2> /dev/null
  find ~/.gradle/wrapper/dists -maxdepth 2 -mindepth 2 -type d 2> /dev/null
} | wc -l | tr -d ' '
