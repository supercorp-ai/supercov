# What do your React tests actually check?

The starter suite renders a checkout, submits it, and exercises an error. **All
15 executable lines run.** It still passes when the displayed total is wrong,
the accessible name describes deletion, the empty-order button is enabled, or
the failure message says payment succeeded.

Four small assertions make those expectations explicit:

| UI behavior | Assertion | Expression it checks |
| --- | --- | --- |
| Exact displayed total | `toHaveTextContent(/^25\.00$/)` | Price formatting inside `<output>` |
| Accessible payment name | `toHaveAccessibleName('Pay for 2 items')` | The `aria-label` template |
| Disabled empty order | `toBeDisabled()` | The `disabled` expression, for quantity zero |
| Useful error message | An anchored text matcher on the alert | The error text expression |

## Reproduce

From the repository root:

```sh
cargo build -p supercov
npm --prefix examples/react-verification ci
TYPESAFE_API_KEY=... npm --prefix examples/react-verification run demo
```

The script measures the before and after suites, assesses assertion coverage
for both with `supercov runs <run> assertions assess`, and checks four
deliberately broken copies of the component. It changes only a temporary copy;
output goes to `recorded/`. Without `TYPESAFE_API_KEY` it skips the assessment.
Set `SUPERCOV_BINARY` to test another built CLI.

Recorded result: the three weak tests reach 100% of lines, 58.3% of the
statements are asserted, and 1 of the 4 UI expressions is judged asserted -- the
broken copies show that none of the four is actually caught, so that one is a
miss. The seven focused tests reach 83.3%, all four expressions are asserted,
and all four broken copies fail.

## Why this adds something to existing coverage

Keep Vitest, Testing Library and your current assertions. Execution coverage
answers whether code ran; assertion coverage asks whether a test would notice
that code going wrong, statement by statement, including JSX expressions. That
makes "what checks this value?" a specific question with an answer per line.
It is a judgment by a model, checked here against the broken copies, not a
proof that a test rejects every possible bug.

`recorded/result.json` and `recorded/ui-assertions.json` contain the results.
This is a reproducible teaching example, not a production-app case study.

Separate compatibility tests in `compat/` check SSR hydration, preservation of
server DOM, the first interaction, and recovery from mismatched HTML in both
jsdom and Chromium. Run `node scripts/react-hydration-compatibility.mjs` from the
repository root after installing dependencies and Chromium. These tests do not
establish Next.js App Router, streaming, or server-action support.
