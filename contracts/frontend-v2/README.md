# Language-frontend protocol v2

Version 2 preserves every v1 producer/engine ownership rule and adds the
`test` transition kind. This represents ordinary test-body execution when a
runner can identify the test phase but cannot causally attribute execution to
an action or an individual assertion.

The v1 contract remains checked in and immutable. Producers must emit the
current protocol version; there is no persisted v1 declaration to migrate
because frontend declarations are not part of evidence archive v2.

See [`../frontend-v1/README.md`](../frontend-v1/README.md) for the full producer
boundary. All requirements there apply unchanged.
