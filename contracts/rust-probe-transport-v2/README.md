# Rust probe transport v2

Status: **candidate wire contract; Rust product frontend private**.

V2 preserves v1's fixed header and descriptor sizes, authentication,
publication and crash semantics. It changes the magic/version, assigns header
offset `80` to a shared atomic phase-invocation sequence, and adds record kind
`4` so a runtime can publish exact dynamic phase identity without guessing a
call graph.

A phase record uses the descriptor context as the child context, the ID payload
as the stable assertion decision ID, and exactly sixteen value bytes as the
little-endian parent context followed by the globally allocated invocation
nonce. Outcome and flags are zero. The child is FNV-1a over the v2 domain,
parent, decision digest and nonce. The reader recomputes it, rejects a mismatch,
rejects context `0` or `u64::MAX`, and rejects any child mapped to two different
parent/decision/nonce triples. Duplicate identical definitions are allowed.

Every non-base observation context must resolve through authenticated phase
records to the supervisor-owned attempt context. Missing parents, cycles,
and cross-attempt parents fail closed. An authenticated definition without a
coverage observation remains valid: an assertion condition may panic before a
verdict is committed, or it may inspect only uninstrumented data. This makes
dynamic, nested and recursive assertion attribution finite and exact without
changing coverage observations or inferring causality from time.
