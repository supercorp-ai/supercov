#!/usr/bin/env bash
# Fetches a file only whole: the download is checked against the checksum its
# host publishes beside it, and a refusal (HTTP 429), a server error, a
# timeout or a truncated or wrong body is tried again after a growing wait,
# never kept. A file the host does not have (404) fails at once.
#
#   fetch-verified.sh <url> <destination> <sha1|sha512>
#
# A destination already there is kept as it is when it matches the checksum
# saved beside it when it was fetched (`<destination>.<algorithm>`), with no
# request at all: a cache hit needs no network.
#
# On 2026-11-23 a JUnit jar fetched without these checks was Maven Central's
# "Too Many Requests" page, and a test read it as a jar with no end header.
set -uo pipefail

url=$1
destination=$2
algorithm=$3
bits=${algorithm#sha}

digest() {
  if command -v "sha${bits}sum" > /dev/null 2>&1; then
    "sha${bits}sum" "$1" | awk '{ print tolower($1) }'
  else
    shasum -a "$bits" "$1" | awk '{ print tolower($1) }'
  fi
}

# `get <url> <file>` prints the HTTP status, 000 where none came back. `-q`
# first: no ~/.curlrc changes what is asked for.
get() {
  curl -q -sSL --connect-timeout 30 --max-time 600 -o "$2" -w '%{http_code}' "$1" 2> /dev/null || true
}

mkdir -p "$(dirname "$destination")"
if [ -s "$destination" ] && [ -s "$destination.$algorithm" ] \
  && [ "$(digest "$destination")" = "$(cat "$destination.$algorithm")" ]; then
  exit 0
fi
want=""
wait=5
for attempt in 1 2 3 4 5 6 7; do
  if [ -z "$want" ]; then
    # The published checksum: the first word of the file beside the
    # download (Maven Central's hold the digest alone, Apache's add a name).
    status=$(get "$url.$algorithm" "$destination.sum")
    [ "$status" = 200 ] && want=$(awk 'NR == 1 { print tolower($1) }' "$destination.sum")
    rm -f "$destination.sum"
  fi
  if [ -n "$want" ]; then
    if [ -s "$destination" ] && [ "$(digest "$destination")" = "$want" ]; then
      echo "$want" > "$destination.$algorithm"
      exit 0
    fi
    status=$(get "$url" "$destination.part")
    if [ "$status" = 200 ] && [ "$(digest "$destination.part")" = "$want" ]; then
      mv "$destination.part" "$destination"
      echo "$want" > "$destination.$algorithm"
      echo "fetched $url"
      exit 0
    fi
    rm -f "$destination.part"
    echo "attempt $attempt: $url answered $status$([ "$status" = 200 ] && echo ', not matching its checksum')"
  else
    echo "attempt $attempt: $url.$algorithm answered $status"
  fi
  if [ "$status" = 404 ]; then
    echo "::warning::$url is not there"
    exit 1
  fi
  [ "$attempt" = 7 ] && break
  sleep "$wait"
  wait=$((wait * 2 > 120 ? 120 : wait * 2))
done
echo "::error::$url could not be fetched whole"
exit 1
