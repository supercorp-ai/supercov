# A host ESM loader claims our generated modules — 2026-09-04

Status: reproduced, not fixed. Found while triaging a report from measuring
Supergateway, whose test command is

```sh
node --test --experimental-loader ts-node/esm --experimental-test-module-mocks tests/**/*.test.ts
```

Every worker fails before test code runs:

```text
SyntaxError: The requested module './capability.js' does not provide an export
named '__supercovBindCapabilityWrapper'
```

## What is happening

The generated runtime lives at `.supercov/node_modules/` inside the project, so
it is inside the host's loader scope. `ts-node/esm` claims those files: the
instrumented workspace's `register.mjs` arrives with TypeScript's
`__rewriteRelativeImportExtension` helper spliced in, which is emit, not our
output. Importing the same directory under that loader from a scratch entry
gives the clearer error:

```text
ERR_REQUIRE_CYCLE_MODULE: Cannot require() ES Module .../capability.js in a
cycle
```

So a host loader pulls our ESM through `require()`, and a named import that
Node links statically has nothing to bind to.

We already declare the generated directory ESM with its own
`{"private":true,"type":"module"}`, and the modules have no cycle among
themselves: `capability.js` imports nothing and `launchSupervisor.js` imports
only Node built-ins. The cycle appears through the loader, not through us.

## What does not fix it

Renaming to an unambiguously-ESM extension. Copying `capability.js` to
`capability.mjs` and importing each under the loader links both fine, so the
extension is not the discriminator. `register.mjs` is already `.mjs` and is
still transformed. Registering the loader twice, which the failing run does,
also links fine in isolation.

## The shape a fix should take

Generated modules are immune to the host's lint policy and its type policy;
they are not immune to its module policy, and they must be. Two candidates:

- Stop depending on link-time named exports between generated modules. If
  `register.mjs` bound the capability wrapper through a runtime property
  lookup rather than a static named import, no host loader could break the
  link, because there would be no link. This is small, but it touches the
  browser-safe seam whose reasoning is documented at the top of
  `capability.js`.
- Or keep the generated runtime outside the project directory so host loaders
  never see it. Larger, and it fights the design that keeps everything under
  `.supercov/`.

## Why it matters more than one project

Any project running a custom ESM loader hits this, and the suite cannot be
measured at all: the failure is at link time, before any test executes. It
also blocks confirming the child-process attribution question in the same
report, because no test ever runs.
