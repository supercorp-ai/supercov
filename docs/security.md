# Security surface

`supercov security` tells you what security-relevant surface each file shows:
twelve named checks, asked file by file, each mapped to the weakness classes it
stands for. There is no score. A file is clean, or it names what fired.

Judgments come from [Jev](https://typesafe.ai), the same way `supercov quality`
gets them. Set `TYPESAFE_API_KEY` as described in [quality](quality.md); reading
a saved assessment never needs a key.

## Start with the repository

```bash
npx supercov security
```

```
Security: 6 of 73 files flagged, 67 clean; 4 confirmed at a line.
  destination_from_input 3, injection_sink 1, missing_authorization 1, unsafe_code_execution 1
Catalog security-v4, model jev-1.13.0, snapshot q_5e1c…

Flagged:
  src/routes/invoices.ts
    0.91  injection_sink
          line 42  0.88  db.query(`select * from invoices where id = ${req.params.id}`)
  src/gateways/sseToStdio.ts
    0.77  destination_from_input  (file-level only)
```

Every line is a claim you can check against the file in seconds. The value is
the model's probability that the pattern is present; nothing is averaged,
because nothing in the evidence supported averaging security answers.

## Two passes, two tiers

Every file is asked the twelve questions once. For the checks that fired at
all, the parser lists every call, literal, route handler and request-body
spread the file has (Python, JavaScript and TypeScript), or every template
expression and configuration value (templates and config files), and the
model says which of the fired checks, if any, each line shows, one Choice
per line. Each classified line then gets the check's own question and a
triage question: would a careful reviewer report this, or dismiss it. A finding confirmed at a line is shown with the line and
the code on it, and with the line where the outside value enters the program
when the model can name one; a finding only the first pass made is marked
file-level only. Three findings of one check within ten lines count as one.
Authorisation findings stay file-level. Whether a guard reaches a handler
and which callers it admits is a fact that lives in the wiring, not the
handler, and no per-line question has been found that decides it: every
attempt measured on the labelled corpus fired on unguarded handlers the
application leaves open by design as often as on the ones it should not.
The audit worklist below carries each entry point with the guards found
reaching it, which is where that judgment belongs.

Measured with this command on fifteen held-out repositories of a
140-repository labelled corpus, whole repositories, at finding level: F1
0.47 with 51% precision and 44% recall, where Semgrep scores 0.14, the
general agentic LLM scanners 0.50 to 0.60, and the two leaders 0.76 and
0.77, at about two cents per repository against 30 cents to 4 dollars for
the agentic scanners. Injection, secrets, path, redirect and mass
assignment are found at 70 to 90% recall; authorisation and authentication
at 10%, because the guard that decides them lives in another file. A file
too large for one request is read in windows at declaration boundaries.

## Across files

The second pass also labels every function of every file, on the same
request: does it take caller-supplied data, does a parameter reach a
dangerous operation unprotected inside it, does it sanitise or authorise what
it receives, and which kind of operation. Supercov then resolves the file's
imports itself, pairs every function that takes outside data with every
imported function that reaches a sink without sanitising, where the caller's
body names the callee, and confirms each pair with one question that carries
both function bodies. A confirmed path is printed as a call site and a sink:

```
Cross-file paths confirmed: 2
  0.79  injection_sink  app/routes/index.js:19 index -> app/routes/allocations.js:11 displayAllocations
  0.70  injection_sink  app/routes/benefits.js:11 BenefitsHandler -> app/data/benefits-dao.js:2 BenefitsDAO
```

The model never sees two files at once until a path is confirmed; the graph
is host code over resolved imports, one hop.

With `--run`, a second kind of edge joins the first: the run's own record of
which tests executed which lines. A sink function and an entry function that
took outside data under the same test are paired and confirmed the same way,
whether or not any import connects them. That is how a handler that reaches
its sink through a lookup table, a registry, a decorator or a framework is
followed; no import graph has that edge, and no trace is attempted. On a
library whose views dispatch to their sinks by name, this took recall of the
labelled weaknesses from 42% to 75%. Without a run, nothing here is asked. Measured on a third of the same
corpus, confirmed paths fired on 0.1% of unlabelled files, and on real CVEs
in mature projects this stage is where the recall has to come from, because
a real vulnerability rarely sits in one file. JavaScript, TypeScript and
Python have parsers for this; other languages get the twelve questions and
the pattern candidates only.

## Finish the audit with your agent

What a file question cannot decide, a reader of the whole application can:
who may reach a handler, whether a role should be allowed an operation,
whether a credential flow is protected. Every assessment writes a worklist
of those questions with the evidence gathered, ranked with unguarded entry
points first, to `.supercov/security/audit.json`:

```bash
npx supercov security audit
npx supercov docs security-agent
```

Your coding agent works the list, reading one item at a time with
`security audit <id>`, which prints the item's own lines, the line that
registers it, and the definitions of the guards and models it names; it
writes a verdict per item and runs `security audit check` before the next
assessment, which names any verdict that would not merge. The next
assessment keeps every verdict whose file is unchanged, re-opens the rest,
and reports three tiers: what the model confirmed, what the agent verified,
what the agent dismissed. `security patch` lists the items a change
touches, so a review re-opens only those.

On sixteen labelled repositories the worklist puts 77% of the labelled
weaknesses within reach of whoever works it, against 43% for the model's
line tier alone. What the agent then finds depends on the agent: measured
passes landed between the mid-tier agentic scanners and the leaders, at the
agent's own cost per token. The worklist costs nothing to produce.

## The twelve checks

| Check | Stands for |
| --- | --- |
| secret_in_source | a credential or signing secret written into the source (CWE-798, 259, 321) |
| injection_sink | a query or command assembled from a caller's value (CWE-89, 78, 77, 943, 90) |
| unsafe_code_execution | eval, exec, pickle or unsafe deserialisation of outside data (CWE-94, 95, 502, 1336) |
| unescaped_output | an outside value written into markup without escaping (CWE-79, 116) |
| path_from_input | a file operation on a caller-controlled path, name or type (CWE-22, 73, 434) |
| missing_authorization | a handler acting on a caller-chosen record with no visible check (CWE-862, 863, 639, 284, 306) |
| weak_authentication | a token, session or password handled in a way that weakens it (CWE-287, 613, 614, 347, 384, 1004) |
| weak_cryptography | a primitive weak for its purpose, or predictable randomness for a secret (CWE-327, 328, 326, 330, 338, 916) |
| sensitive_data_exposure | a secret, token or internal detail in a log or a response (CWE-200, 209, 532, 312) |
| destination_from_input | a request or redirect going wherever a caller's value says (CWE-918, 601) |
| unchecked_mass_assignment | a request body copied wholesale into a record (CWE-915, 1321) |
| insecure_configuration | a provided protection switched off or weakened, or debug mode left on (CWE-16, 295, 352, 611, 942, 1004, 489, 215) |

Each asks whether a pattern is present, never whether it is exploitable, and
each states its exception. A value is from outside the program when it arrives
at run time from a caller, request, message, upload or third-party service; the
operator's own configuration, arguments, environment and shipped files are not.

## What is known about each check

Measured on 2026-09-20 against constructed pairs, a clean floor, and RealVuln,
a corpus of 140 deliberately vulnerable repositories with 3,381 labelled
findings in Python, TypeScript and JavaScript:

- every check caught its constructed positive at 0.97 or above and stayed quiet
  on a matched safe file, both when asked of a file and of a change;
- on the labelled corpus, the check a finding's weakness class maps to fired on
  68% of files carrying a finding, from 96% for secrets in source down to 43%
  for missing authorisation, and on 5 of 76 certified false-positive traps;
- on the same files and labels, Semgrep reached 10%, SonarQube 18% and Snyk 21%.

`missing_authorization` is the weak one and is shown with that number. A
per-file question cannot see a guard in another file, so it is right seven
times in ten on code people wrote and less on generated corpora that label
single unguarded routes inside otherwise guarded files.

```bash
npx supercov security file src/routes/invoices.ts
```

shows every check with its value and its evidence line, so a finding can be
weighed without leaving the terminal.

## Cross it with a run

```bash
npx supercov security --run latest
```

adds, for every flagged file, whether the saved coverage run executed it and how
many of its measured lines no test reached. A handler that builds a query from
request input is one thing; the same handler that no test ever executes is a
stronger claim, and one that needs both halves of this product to make.

## What a change introduced

```bash
npx supercov security patch --base origin/main
npx supercov security patch --annotate github
```

The same twelve, asked whether the new version shows a surface the old one did
not. `quality patch` asks them too, alongside its own catalog, so a review of a
change needs one command; `security patch` is the same answers without the
complexity findings.

## Reference

```bash
npx supercov security                             # this repository
npx supercov security gaps                        # only files something fired on
npx supercov security file src/server.ts          # one file, every check, with evidence
npx supercov security scope                       # which files, and why
npx supercov security snapshots                   # saved assessments
npx supercov security diff <older> <newer>        # what appeared
npx supercov security audit                       # the worklist for your agent
npx supercov security audit <id>                  # one item with its evidence inline
npx supercov security audit check                 # validate verdicts before they merge
npx supercov security patch                       # what a change introduced, and which items it touches
npx supercov security --run latest                # flagged and untested
```

Cache and snapshots live under `.supercov/security/`, apart from quality's, so
one can never be read as the other.
