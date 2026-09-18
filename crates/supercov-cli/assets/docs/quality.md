# Understanding quality

Coverage answers whether your tests exercised the code. Quality answers a
different question: what is in the code, named property by named property, so
you can check each answer against the file yourself.

Judgments come from [Jev](https://typesafe.ai), which answers typed questions —
yes or no, a position on a scale, a choice among options — rather than generating
prose. Supercov asks twelve yes/no questions per file and does the arithmetic
itself.

It is **experimental and advisory**. Nothing it reports fails a command.

## It asks twelve definite questions, and does the arithmetic itself

A quality score usually comes from asking a model to place a file on a scale.
That approach was tried here and abandoned: on 272 Java classes rated by
professionals it ordered pairs correctly 90.2% of the time, and **counting the
statements in the file got 90.3%**. A paid call cannot justify itself by
matching `wc`.

So instead, twelve yes/no questions about specific named properties, drawn from
Fowler and Beck's refactoring smells and the class-scope smells CodeScene's Code
Health is built from:

god class, long method, deep nesting, complex conditional, long parameter list,
duplicated logic, primitive obsession, dead code, feature envy, temporary field,
message chains, magic values.

**The number is arithmetic this CLI does over those twelve answers, not
something the model was asked for.** That is the point. A single opaque grade
gives a reader nothing to argue with. Twelve named claims can each be checked
against the file in seconds, and `quality file <path>` prints all of them with
what is known about each.

## What the number is worth

On the same 272 classes, restricted to pairs whose statement counts are close so
size cannot decide the answer:

| Predictor | orders size-matched pairs correctly |
| --- | ---: |
| this composite | **78%** |
| CodeScene Code Health | 67% |
| statement count | 58% |
| Microsoft Maintainability Index | 52% |
| cyclomatic complexity | 44% |

Asked twice, a single check moves by a median of 0.01 and the composite by 0.5%
of its range.

Three limits belong beside those numbers.

- The composite **correlates 0.92 with a statement count**. It is largely a size
  measure whose residual is right, which is a weaker claim than the table sounds.
- Its margin over Code Health is **somewhere between 7 and 11 points**, moving by
  several points depending on which fifth of the corpus is dropped. The margins
  over a statement count and the Maintainability Index are large and stable.
- **Six checks score as well as twelve.** These are not twelve measurements but
  one measurement taken twelve times, so rewording checks does not move the
  number. Four were reworded after a blind reader found them failing, and the
  composite moved by 0.4 with an interval that includes zero.

## Bands, not decimals

Text output reports `good`, `fair` or `weak` rather than a number, because the
measured resolution of these judgments is about one point and `4.4/10` would
claim precision nobody observed. The number is in `--json`, where something has
to sort.

A property counts as present at 0.60, chosen against three references rather
than taken as a midpoint. **Health does not use that threshold**: the score is
the mean of the raw answers, so it decides what is shown and never what is
scored.

## What a change introduced

```bash
supercov quality patch --base origin/main
```

The same twelve checks asked differentially — does the new version show this
where the old one did not — plus **seven risk checks that exist only in this
form**: a credential written into source, untrusted input interpolated into a
query, a change to who may do what, a test that now checks less, a schema or
data migration, and debugging left behind.

Both whole versions go into one request. A hunk cannot separate a property a
change introduced from one the file already had, which is the entire question.

### What it covers, and what it does not

This reports **structure**, and structure is a corner of what a reader of a
change cares about. Measured against 173 comments real reviewers wrote on 50
pull requests from cal.com, Discourse, Grafana, Keycloak and Sentry, the twelve
complexity properties have a word for 8% of them. Fifty-four percent of those
comments are about bugs, and **nothing here asks about correctness**.

So it is a structural regression check, in the same family as CodeScene's
decline gate: it tells you a change made code harder to follow. It is not
looking for the race condition, and it does not replace the person who is.

The seven risk checks were built for that gap and are cleaner: on constructed
positives with matched safe changes they separate by 0.91 or more, and across
the same 50 real pull requests they fire about once per pull request. On the
seven where a reviewer raised a security concern, one fired on four, against
nine of the forty-three without one.

A seventh, `breaks_api`, was removed: it caught its constructed positive cleanly
but fired on 20 of 50 real pull requests with a median of 0.45. Changing an
exported signature is ordinary in library work, so it was announcing a common
event rather than catching a rare one.

## Which files are assessed

The same shape as the coverage source scope, and the same vocabulary, so
`runs patch` and `quality patch` describe one set of files. Every file is
**included**, **excluded** or **ambiguous**.

Roots come from package manifests and from what a manifest declares: `main`,
`bin` and `exports` in a `package.json`, `path` entries and `build.rs` in a
`Cargo.toml`. Inside each package, the conventional source directories. Test
code, generated output, tool scripts, examples, benchmarks and documentation are
excluded, each with its reason.

Anything first-party under no recognised root is **ambiguous**, never silently
dropped. When a credential is available, one extra request asks the model about
exactly those files, with the whole tree as context and no file contents. On six
repositories in six languages that agreed with the conventions on 99.1% of the
1,503 files they decide, and settled the 115 they cannot: it separated a shipped
runtime tree from prototype crates exactly.

`SUPERCOV_SOURCE_ROOTS=src,app` declares them yourself, and a declaration is
never re-decided.

```bash
supercov quality scope
```

## Comparing two assessments

```bash
supercov quality diff <older> <newer>
```

Which files lost health, which gained, which properties appeared, and which
entered or left the scope. Only snapshots of the same catalog version and model
can be compared. A file whose bytes are identical in both is marked, because the
same question was asked twice and any movement is the model's own variation
rather than an edit worth hunting for.

## Crossing a change with coverage

```bash
supercov quality patch --base origin/main --run latest
```

Marks any changed file where a property appeared **and** the run left measured
lines uncovered. Neither half justifies stopping anyone alone. Both at once
describes a change that made code harder to follow where no test runs.

Quality reads coverage. **Coverage never reads quality**, and nothing about
quality appears in a run's own output, because an assessment costs money and
needs a credential.

## Cost and privacy

An assessment sends source to [TypeSafe](https://typesafe.ai) and needs
`TYPESAFE_API_KEY`. A whole
repository of 197 files costs about **$0.02** and takes under half a minute at
eight concurrent requests; a changed file costs about $0.0005. Responses are
cached under `.supercov/quality/requests/` by exact request hash, so a re-run
with no edits sends nothing.

`--dry-run` prints the exact request bodies and contacts nothing.

## Reference

Full option lists are in the [CLI reference](https://supercov.com/docs/cli).

```bash
supercov quality                             # this repository
supercov quality gaps                        # only files something fired on
supercov quality file src/server.ts          # one file, every check
supercov quality scope                       # which files, and why
supercov quality snapshots                   # saved assessments
supercov quality diff <older> <newer>        # what declined
supercov quality patch --base origin/main    # what a change introduced
```
