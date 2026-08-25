# Language-frontend protocol v2

Version 2 preserves every v1 producer/engine ownership rule and adds the
`test` transition kind. This represents ordinary test-body execution when a
runner can identify the test phase but cannot causally attribute execution to
an action or an individual assertion.

The v1 contract remains checked in and immutable. Producers must emit the
current protocol version; there is no persisted v1 declaration to migrate
because frontend declarations are not part of evidence archive v2.

## Product ownership policy

The protocol can represent `native-import` and `mixed` declarations so that
development conformance harnesses can pass oracle facts through the same
validator and analyzer. Those structural sources are not product execution
modes. Every declaration produced by a user run must use `owned-probes`:
Supercov discovers and injects its own instrumentation from the existing test
command without invoking or requiring an external coverage engine.

Oracle importers are compile-gated development infrastructure. They may emit
fixtures for differential tests, but product orchestration must never select
them and distribution packages must not depend on their external engines.

See [`../frontend-v1/README.md`](../frontend-v1/README.md) for the full producer
boundary. All requirements there apply unchanged.
