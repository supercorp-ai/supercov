# Understanding quality

`supercov quality` tells you what is in your code: twelve named properties,
checked file by file, so a score is never a number you have to take on faith.

Judgments come from [Jev](https://typesafe.ai), which answers typed questions
rather than generating prose. Supercov asks the questions and does the
arithmetic, so every part of a score is a claim you can check against the file.

## Set your key

Assessing needs a TypeSafe API key, read from `TYPESAFE_API_KEY`. Get one at
[typesafe.ai](https://typesafe.ai).

```bash
export TYPESAFE_API_KEY=...        # for this shell
TYPESAFE_API_KEY=... npx supercov quality   # for one command
```

The environment variable is the only way to pass it. A key given on the command
line ends up in your shell history and in the process list, where anyone on the
machine can read it.

Set it the way your environment already sets secrets:

| Where | How |
| --- | --- |
| GitHub Actions | `env: { TYPESAFE_API_KEY: ${{ secrets.TYPESAFE_API_KEY }} }` |
| GitLab CI | a masked CI/CD variable named `TYPESAFE_API_KEY` |
| Docker | `docker run -e TYPESAFE_API_KEY ...` |
| Local development | a `.env` loaded by `direnv`, `dotenv` or your shell profile |

Reading a saved assessment never needs a key, and `--dry-run` prints the exact
requests without sending them.

## Start with the repository

```bash
npx supercov quality
```

No arguments, no configuration. It finds your source, asks twelve questions of
each file, and saves a snapshot you can read afterwards without a key or a
network.

```
Quality fair (5.0/10, weighted by size) over 163 files, 5147399 bytes.
  11 good, 53 fair, 99 weak.

Weakest:
  weak  src/lib/modernHttp.ts
        long_method 0.93, duplicated_logic 0.88, complex_conditional 0.87, +7 more
```

The band is the headline because the underlying resolution is about a point:
`good` is 8 and above, `fair` is 5 to 8, `weak` is below 5. `--json` carries the
number when something needs to sort.

## The twelve properties

god class, long method, deep nesting, complex conditional, long parameter list,
duplicated logic, primitive obsession, dead code, feature envy, temporary field,
message chains, magic values.

They come from Fowler and Beck's refactoring smells and the class-scope smells
CodeScene's Code Health is built from. Each is a yes/no question with a
definition and a stated exception, so two careful readers would agree on the
answer.

Three of them fire on more than half the files in a typical repository, so the
summary shows the three strongest per file. To see all twelve with what is known
about each:

```bash
npx supercov quality file src/lib/modernHttp.ts
npx supercov quality gaps        # only files something fired on
```

## Reading the score

Health is the mean of the twelve answers, done by this command rather than by
the model. Directory and repository health weight files by size, so a folder of
one-line re-exports cannot outvote the file everything depends on.

Two things are worth knowing before you act on it.

**It moves with size.** Bigger files score worse, and that is mostly right, but
it means the score rarely surprises you on a file you already knew was large.
The interesting cases are the small files that score badly.

**It measures structure, not correctness.** Nothing here asks whether the code
works. Use it to find code that is hard to change, not code that is wrong.

## What a change introduced

```bash
npx supercov quality patch
```

With no range it reviews your uncommitted work when the tree is dirty, and
everything since your branch left its default branch when it is clean. Say so
explicitly with `--unstaged`, `--staged`, or `--base origin/main`, which uses
the merge base so commits other people landed after you branched are not
counted as yours.

It asks the twelve properties differentially, whether the new version shows
something the old one did not, and adds six checks that only make sense for a
change:

- a credential written into source
- untrusted input interpolated into a query
- a change to how the system decides who may do what
- a test that now checks less than it did
- a database schema or data migration
- debugging left behind

Output lists only files where something appeared. A change that introduces
nothing says so in one line.

```bash
npx supercov quality patch --base origin/main --annotate github
```

`--annotate github` prints workflow annotations on stdout. It needs no token and
posts no comment.

This is a structural check on your change. It will tell you a function got
harder to follow. It will not find the race condition, and it does not replace
the person who would.

## Which files get looked at

```bash
npx supercov quality scope
```

Source roots come from your package manifests and from what they declare, so a
`bin` or `exports` entry counts even when it is not in a conventional directory.
Test files, generated output, tool scripts, examples, benchmarks and
documentation are left out, each with its reason. `--all` includes everything.

If some of your code sits somewhere none of that recognises, Supercov asks Jev
about those paths with your whole tree as context, and says how many it decided
that way. To decide yourself:

```bash
SUPERCOV_SOURCE_ROOTS=src,packages npx supercov quality
```

A declaration is never second-guessed.

## Tracking it over time

```bash
npx supercov quality snapshots
npx supercov quality diff <older> <newer>
```

Which files lost health, which gained, which properties appeared, and which
files entered or left the scope. A file whose contents did not change is marked,
so a small movement does not send you looking for an edit that was never made.

## What it costs

Jev charges for what it reads and nothing for what it writes, so the bill is the
size of your source. At $0.042 per million input tokens:

| | files | cost |
| --- | ---: | ---: |
| a small library | 25 | $0.003 |
| a typical service | 200 | $0.02 |
| a large monorepo | 2,000 | $0.20 |
| one changed file in a review | 1 | $0.0005 |

The command prints its estimate before sending anything, so a number that looks
wrong can be stopped rather than discovered on an invoice:

```
[supercov] quality: 35 requests, about 111840 input tokens ($0.0047) if none is cached
```

**Most runs cost far less than that estimate.** Answers are cached by content
under `.supercov/quality/requests/`, so a second run pays only for files that
actually changed. The cache follows content rather than paths, which means
switching branches, rebasing or checking out an old commit reuses everything
unchanged: assessing the same directory 400 commits back in a real repository
answered every file from cache and sent nothing.

Three ways to spend less:

- **Assess a directory, not the tree**, while you are iterating:
  `npx supercov quality src/api`.
- **Review the change, not the repository**, in CI: `npx supercov quality patch`
  costs about $0.0005 per changed file, so a typical pull request is a fraction
  of a cent.
- **Keep `.supercov/` between CI runs** if your runner supports a cache. An
  unchanged file then costs nothing on every run after the first.

`--refresh` asks again and bypasses the cache. `--dry-run` prints the exact
requests, sends nothing and costs nothing.

## Reference

```bash
npx supercov quality                             # this repository
npx supercov quality gaps                        # only files something fired on
npx supercov quality file src/server.ts          # one file, every check
npx supercov quality scope                       # which files, and why
npx supercov quality snapshots                   # saved assessments
npx supercov quality diff <older> <newer>        # what declined
npx supercov quality patch                       # what a change introduced
```

Full options are in the [CLI reference](https://supercov.com/docs/cli).
