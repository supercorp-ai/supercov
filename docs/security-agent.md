# Auditing security with an agent

Use these instructions when asking a coding agent to finish a security audit
that `supercov security` began. The model behind `supercov security` decides
what a single file shows, cheaply and for every file. What it cannot decide is
who may reach a handler, whether a role should be allowed an operation, and
whether a credential flow is protected: facts that live in the wiring, the
data model and the application's intent. For those it writes a worklist, with
the evidence gathered, and the agent answers.

The result is one report with three tiers: what the model confirmed, what the
agent verified, and what the agent dismissed, each with its evidence.

## Run the assessment and read the worklist

```sh supercov-example
npx supercov security
npx supercov security audit --json
```

The worklist is the file `audit` names, `.supercov/security/audit.json`. Keep
the snapshot the assessment wrote fixed while working. Do not change
application code or tests unless the user also asked for that. Never edit an
item's `basis`; it is the hash of the file the item was cut from, and a verdict
on a changed file is re-opened on the next assessment.

## What an item is

Each item is a question with its evidence gathered, never a finding:

- `entry`: a registered entry point, with the guards found reaching it in its
  body, its decorators or its registration line; the models it touches; and
  the repository's guards, roles and permission names on the first item under
  `repository`. Decide which callers can reach it, whether what it reads,
  changes or returns needs a check of who the caller is or what they may do,
  and whether every admitted role should be allowed this operation on this
  record.
- `credential`: a login, registration, reset, token or code flow. Decide
  whether it is rate limited or lockout-protected, whether it reveals whether
  an account exists, and whether what it issues is single-use, bound to the
  caller and expiring.
- `line`: a line the model put between 0.3 and the cut for a named check, or
  dismissed on triage. Decide whether it is a weakness worth fixing and where
  the outside value enters.
- `logging`: a request-derived value written to a log. Decide whether it can
  carry control characters into the log or private data into a log store.

Items are ranked: unguarded entry points first, then credential flows, then
uncertain lines, then logging. Read the file and the definitions the context
names; use ordinary source-reading tools for the rest.

## Read one item with its evidence inline

```sh supercov-example
npx supercov security audit entry:api_views/users.py:179
```

prints the item's own lines with their numbers, the registration line that
names it, and the definition of every guard and model the item names, so
the common case is decided without opening a file. Open a file only when
the item's context does not decide it.

## Write verdicts

Fill `verdict` on each item you settle:

```json
{
  "finding": true,
  "check": "missing_authorization",
  "cwe": "CWE-862",
  "line": 180,
  "evidence": "document_decide checks can_view_patient only when user.is_clinician; a front-desk user reaches it through staff_required and can verify any document",
  "reason": "The scope check applies to one role; the other admitted role is unchecked."
}
```

- `finding` false dismisses the item; give the `reason` all the same, naming
  the guard, role check or design decision that settles it.
- `check` is one of the twelve check ids; `cwe` the most specific class.
- `line` is where the weakness is, which may differ from the item's line.
- `evidence` names the lines that decide it, in words a reviewer can verify.
- Keep what you could not settle in `questions`, one sentence each; an item
  with open questions stays open.

Precision rules, learned from agents over-reporting:

- An application with no authentication anywhere is one design finding, on
  the entry point that reads or changes the most sensitive data, not a
  finding per handler. Report a missing check per handler only where the
  application authenticates elsewhere and this handler is the exception, or
  where the handler reaches another person's record, credentials, money or
  an administrative operation.
- Use the most specific CWE: cookie without Secure is CWE-614, without
  HttpOnly CWE-1004, CSRF CWE-352, a hardcoded key CWE-321, a hardcoded
  password CWE-798, user enumeration CWE-204, no attempt limit CWE-307.
  A broad class where a specific one exists is counted as a miss.
- Dismiss a line the model raised only when the code makes it safe, not the
  environment: a stack-trace handler gated on a development flag is still a
  finding if the flag can be on in production.
- When the same weakness has a definition site and a use site, file it where
  a fix goes (the definition), and name the use in `evidence`.

Use a single writer for the file. Save atomically if your editor supports it.

## Check and read the merged report

```sh supercov-example
npx supercov security audit check
npx supercov security
```

`audit check` names every verdict that would not merge: a check id that is
not one of the twelve, a line outside the file, missing evidence, a finding
without a CWE, and every item whose file changed since the worklist was
written. Fix those before the run. The next `security` run keeps every
verdict whose file is unchanged, re-opens the rest, and reports the three
tiers together; agent findings show `[agent; registered at file:line]`.
