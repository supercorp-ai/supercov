#!/usr/bin/env bash
# Wait for CI cheaply: one GraphQL query every three minutes, and a line only
# when something changed.
#
#   scripts/wait-ci.sh <pull request number | URL>   # every check on its head commit
#   scripts/wait-ci.sh <run id | run URL>            # one workflow run
#
# A number means the repository of the current directory; a URL names its
# own. The first line says which repository it waits on.
#
# Exits 0 when everything passed, 1 when anything failed or was cancelled, 2
# on a usage error. Prints `STALLED` when nothing finished for ten minutes,
# and keeps waiting.
#
# Why not `gh run watch` or a `gh run view` loop: both use the REST API, which
# allows 5,000 requests an hour for everyone using the token. `gh run watch`
# asks every three seconds, and a loop over jobs asks once per job. Several
# agents waiting that way used up the hour's allowance in minutes. This asks
# GraphQL, which has its own allowance, once per poll however many jobs there
# are, and it waits longer when that allowance runs low.
set -uo pipefail

target=${1:-}
[ -n "$target" ] && [ $# -eq 1 ] || { sed -n '4,5p' "$0" | sed 's/^# *//' >&2; exit 2; }
INTERVAL=180
STALL_SECONDS=600

if [[ $target =~ ^https://github\.com/([^/]+/[^/]+)/ ]]; then
  repository=${BASH_REMATCH[1]}
else
  repository=$(gh repo view --json nameWithOwner -q .nameWithOwner) || exit 2
fi
owner=${repository%/*}; name=${repository#*/}

FAILED='FAILURE CANCELLED TIMED_OUT ACTION_REQUIRED STARTUP_FAILURE ERROR'
PENDING='QUEUED IN_PROGRESS PENDING WAITING EXPECTED REQUESTED'

if [[ $target =~ ^[0-9]{1,7}$ || $target =~ /pull/([0-9]+) ]]; then
  kind=pr
  number=${BASH_REMATCH[1]:-$target}
  echo "waiting on $repository#$number"
  # Checks come 100 to a page; a pull request with more is read page by page.
  query='query($owner: String!, $name: String!, $number: Int!, $after: String) {
    rateLimit { remaining resetAt }
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { state commits(last: 1) { nodes { commit {
      oid statusCheckRollup { state contexts(first: 100, after: $after) {
        totalCount
        checkRunCountsByState { state count }
        statusContextCountsByState { state count }
        pageInfo { hasNextPage endCursor }
        nodes { ... on CheckRun { name status conclusion isRequired(pullRequestNumber: $number) } ... on StatusContext { context state isRequired(pullRequestNumber: $number) } }
      } } } } } } } }'
  arguments=(-F number="$number")
else
  kind=run
  [[ $target =~ ([0-9]{8,}) ]] || { echo "not a pull request number, run id or run URL: $target" >&2; exit 2; }
  query='query($url: URI!) {
    rateLimit { remaining resetAt }
    resource(url: $url) { ... on WorkflowRun { checkSuite { status conclusion
      checkRuns { totalCount }
      done: checkRuns(filterBy: {status: COMPLETED}) { totalCount }
      failed: checkRuns(first: 20, filterBy: {conclusions: [FAILURE, CANCELLED, TIMED_OUT, ACTION_REQUIRED, STARTUP_FAILURE]}) { nodes { name } } } } } }'
  arguments=(-f url="https://github.com/$repository/actions/runs/${BASH_REMATCH[1]}")
  echo "waiting on $repository run ${BASH_REMATCH[1]}"
fi

last=""; changed=$(date +%s); reported=""
while true; do
  answer=$(gh api graphql -f query="$query" -f owner="$owner" -f name="$name" "${arguments[@]}" 2>&1) || {
    echo "query failed, trying again in ${INTERVAL}s: $(head -c 200 <<<"$answer")"; sleep "$INTERVAL"; continue; }
  if [ "$kind" = pr ]; then
    # Read the rest of the checks and add their nodes to the first page's.
    page=$answer
    while [ "$(jq -r '.data.repository.pullRequest.commits.nodes[0].commit.statusCheckRollup.contexts.pageInfo.hasNextPage // false' <<<"$page")" = true ]; do
      after=$(jq -r '.data.repository.pullRequest.commits.nodes[0].commit.statusCheckRollup.contexts.pageInfo.endCursor' <<<"$page")
      page=$(gh api graphql -f query="$query" -f owner="$owner" -f name="$name" -f after="$after" "${arguments[@]}" 2>&1) || break
      answer=$(jq --argjson more "$(jq '.data.repository.pullRequest.commits.nodes[0].commit.statusCheckRollup.contexts.nodes' <<<"$page")" \
        '.data.repository.pullRequest.commits.nodes[0].commit.statusCheckRollup.contexts.nodes += $more' <<<"$answer")
    done
  fi

  if [ "$kind" = pr ]; then
    summary=$(jq -r --arg failed "$FAILED" --arg pending "$PENDING" '
      .data.repository.pullRequest as $pr
      | ($pr.commits.nodes[0].commit) as $commit
      | ($commit.statusCheckRollup.contexts // {totalCount: 0, checkRunCountsByState: [], statusContextCountsByState: [], nodes: []}) as $c
      | ($c.checkRunCountsByState + $c.statusContextCountsByState) as $counts
      | def total($states): [$counts[] | select(.state as $s | $states | split(" ") | index($s)) | .count] | add // 0;
      [ $commit.oid[0:8],
        $c.totalCount,
        total($pending),
        # When branch protection names required checks, they decide: a
        # job another event skipped leaves its earlier, cancelled run behind
        # with nothing of its name to replace it. Otherwise a check counts as
        # failed only when no check of its name passed or is still running
        # (a run a newer one replaced shows as cancelled).
        # A commit status and a job can share a name (the `Slow tier` status
        # and the job that posts it): grouped apart, a passed job never hides
        # a failed status.
        ([$c.nodes[] | {name: (.name // .context), kind: (if .context then "status" else "check" end), result: (.conclusion // .status // .state // ""), required: (.isRequired == true)}]) as $checks
        | (if any($checks[]; .required) then [$checks[] | select(.required)] else $checks end) as $deciding
        | ($deciding | group_by([.kind, .name])
          | map(select(any(.[]; .result as $s | $failed | split(" ") | index($s))
                       and (any(.[]; .result as $s | "SUCCESS NEUTRAL SKIPPED \($pending)" | split(" ") | index($s)) | not)) | .[0].name)
          | join(", ")),
        ([$deciding[] | select(.result as $s | $pending | split(" ") | index($s))] | length),
        (any($checks[]; .required)),
        ($commit.statusCheckRollup.state // "NONE"),
        $pr.state ] | map(tostring) | join("\u001f")' <<<"$answer")
    IFS=$'\x1f' read -r head total pending failed deciding_pending required verdict state <<<"$summary"
    progress="$head: $((total - pending))/$total checks done"
    finished=""
    if [ "$total" -gt 0 ] && [ "$pending" -eq 0 ]; then finished=yes; fi
    # The required checks have their answer: the rest cannot change it.
    if [ "$required" = true ] && [ "$deciding_pending" -eq 0 ]; then finished=yes; fi
    if [ "$state" != OPEN ]; then finished=yes; fi
  else
    summary=$(jq -r '
      .data.resource.checkSuite as $suite
      | [ $suite.checkRuns.totalCount,
          $suite.done.totalCount,
          ([$suite.failed.nodes[].name] | join(", ")),
          ($suite.conclusion // "NONE"),
          $suite.status ] | map(tostring) | join("\u001f")' <<<"$answer")
    IFS=$'\x1f' read -r total done failed verdict state <<<"$summary"
    progress="run: $done/$total jobs done"
    finished=$([ "$state" = COMPLETED ] && echo yes)
  fi

  if [ "$progress|$failed" != "$last" ]; then
    echo "$progress${failed:+; failed: $failed}"
    last="$progress|$failed"; changed=$(date +%s); reported=""
  elif [ -z "$reported" ] && [ $(( $(date +%s) - changed )) -ge "$STALL_SECONDS" ]; then
    echo "STALLED: $progress, nothing finished in $((STALL_SECONDS / 60)) minutes"; reported=yes
  fi

  if [ "$finished" = yes ]; then
    [ "$kind" = pr ] && [ "$required" = true ] && verdict="required checks $([ -z "$failed" ] && echo passed || echo failed)"
    echo "finished: $verdict"
    [ -n "$failed" ] && exit 1
    # A pull request's rollup counts a replaced, cancelled run against it;
    # the failed list above already leaves those out.
    [ "$kind" = pr ] || [[ $verdict =~ ^(SUCCESS|NEUTRAL|SKIPPED)$ ]] || exit 1
    exit 0
  fi

  remaining=$(jq -r '.data.rateLimit.remaining // 5000' <<<"$answer")
  wait=$INTERVAL
  # Under a tenth of GraphQL's allowance left: wait for it to come back.
  if [ "$remaining" -lt 500 ]; then
    reset=$(jq -r '.data.rateLimit.resetAt' <<<"$answer")
    until_reset=$(( $(date -j -u -f '%Y-%m-%dT%H:%M:%SZ' "$reset" +%s 2>/dev/null || date -u -d "$reset" +%s) - $(date +%s) ))
    [ "$until_reset" -gt "$wait" ] && wait=$until_reset
    echo "GraphQL allowance low ($remaining left): next poll in $((wait / 60)) minutes"
  fi
  sleep "$wait"
done
