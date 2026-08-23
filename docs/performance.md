# Performance and storage

Supercov separates timings that it can measure safely from overhead that
requires an explicit control run. It never runs a user's command a second time
automatically: an arbitrary test command can write data, call paid services, or
be intentionally non-repeatable.

## Per-run measurements

Every coverage run prints and stores monotonic durations for:

| Phase | Includes |
| --- | --- |
| `initializationMs` | recovery, locking, project discovery, and integrity fingerprints |
| `workspacePreparationMs` | transactional refresh of the isolated namespace |
| `adapterSetupMs` | generated adapters, configs, manifests, and runtime files |
| `instrumentedBuildMs` | the coverage-aware build or direct instrumentation pass |
| `testCommandMs` | the user's unchanged command, including any runner or remote infrastructure latency |
| `reportPreparationMs` | evidence collection, analysis, HTML/JSON generation, and report staging |

The fields are stored in `.supercov/runs/<run-id>/run.json` and returned by
`supercov runs --json`. Total duration is stored separately as `durationMs`.

The non-test phases are not automatically labelled “overhead.” For example, a
test script that ordinarily performs its own build may overlap work with the
instrumented-build phase. True end-to-end overhead must compare equivalent
cold runs or equivalent warm runs of the same command.

## Reproducible comparison

Run the command without and with Supercov under the same cache state. Use at
least three alternating pairs and report the medians. Never compare a cold VM,
browser, package-manager, or build-cache run with a warm one.

```sh
/usr/bin/time -p npm test
/usr/bin/time -p npx supercov -- npm test
```

Package acquisition is a separate user-interface cost. A cold `npx` download
depends on the registry and network; a cached `npx` resolution should be
reported separately from Supercov's recorded phases.

## Reference measurement, not a guarantee

On 2026-08-24, the 29-test Essential SEO offline suite on the development Mac
produced this warm pair:

| Measurement | Duration |
| --- | ---: |
| unchanged command | 39.57 s |
| Supercov total | 45.38 s |
| end-to-end difference | +5.81 s (+14.7%) |
| initialization | 0.06 s |
| workspace preparation | 0.35 s |
| adapter setup | 0.05 s |
| instrumented build | 4.87 s |
| test command inside Supercov | 39.60 s |
| report preparation | 0.40 s |

The test-command durations were effectively identical in this pair. The extra
instrumented build accounted for about 84% of the measured difference. A
seven-sample isolated workspace refresh had a 270 ms median and 447 ms maximum.
The built output grew from 2,781,273 to 3,094,366 logical bytes (+11.3%). A
cached `npx supercov help` added a 686 ms median over direct CLI startup; the
first observed `npx` resolution took 2.22 s.

Cold VM-image runs were 170.34 s without Supercov and 175.44 s with Supercov in
the same session, but a single cold pair is too noisy for a general percentage.
Both spent approximately 124 seconds preparing their VM image.

The latest coverage run retained 4.5 MB of reports and 1.7 MB of raw evidence;
its reusable physical cache occupied 32 MB. These numbers are application- and
filesystem-specific and exist to establish an optimization baseline.

## Isolation strategy trade-offs

| Strategy | Arbitrary-runner compatibility | Failure isolation | Startup/storage |
| --- | --- | --- | --- |
| Transactional physical namespace | Highest; ordinary filesystem consumers and opaque mounts see real files | Strong when staging, publication, recovery, locking, and same-filesystem renames are enforced | Recreates directory entries and may copy bytes when reflinks are unavailable |
| Node loader/Vite plugins | High for observed Node and bundler graphs, incomplete for native readers and hidden remote mounts | Strong because transformed source need not be persisted | Lowest retained storage and usually fastest |
| FUSE/OS overlay | Potentially broad local read interception, but not portable or zero-install | Adds mount, privilege, kernel/extension, and teardown failure boundaries | Low duplicated storage but operationally expensive |
| Adaptive hybrid | Fast path where capability is proven; transactional namespace otherwise | Inherits the physical fallback's guarantees when detection is conservative | Best practical balance; more implementation paths must be tested |

The safe default remains the transactional physical namespace. The intended
optimization is an adaptive hybrid that selects a proven loader/plugin path
and lazily materializes the same transactional fallback whenever an opaque
runner needs real files. FUSE is not an appropriate portable default for a
zero-install `npx` tool.

