#!/bin/sh
# Rust compiled to wasm under JavaScript tests, for validation against
# cargo-mutants: gl-matrix-wasm (a port of gl-matrix to Rust with
# wasm-bindgen), whose own Mocha-style specs test the wasm.
#
#   sh prepare.sh DIR
#
# Clones it into DIR at a fixed commit and makes its specs run in Node, as
# they ran in Chrome under Karma, with node:test's describe, it and hooks:
# the crate built for wasm32 and bound with `wasm-bindgen --target nodejs`
# (the specs' `init()`, the web glue's, is not needed there), the
# TypeScript specs transpiled to CommonJS with
# esbuild (the helper reading an object's pointer by its current name), and
# wasm-bindgen moved to the CLI's version without the nightly
# feature. `npm test` is the test command both tools run: the wasm built
# (build.sh) and vec2's spec run against it, but for the 24 tests that pass
# a vector as both input and output, which wasm-bindgen 0.2.129 refuses
# (`recursive use of an object`).
set -e
dir=$1
here=$(cd "$(dirname "$0")" && pwd)
git clone -q https://github.com/dtysky/gl-matrix-wasm "$dir"
cd "$dir"
git checkout -q e84418b547693d69893188818e6e71f41f8e0bf8
# The crate in a directory of its own beside the JavaScript, as Supercov
# measures Rust compiled to wasm under JavaScript tests.
mkdir rust
git mv Cargo.toml src rust/
git rm -q --cached Cargo.lock && rm Cargo.lock
sed -i.orig -e 's/^version = "0.2.45"/version = "=0.2.129"/' -e '/^features = \[/,/^\]/d' rust/Cargo.toml
rm -f rust/Cargo.toml.orig
cat > package.json <<'JSON'
{ "name": "gl-matrix-wasm-specs", "private": true, "scripts": { "test": "sh build.sh && node --test --test-reporter=dot --test-skip-pattern='is the output vector' spec-js/vec2-spec.js" }, "devDependencies": { "esbuild": "0.25.10" } }
JSON
npm install --no-audit --no-fund --loglevel error
mkdir -p spec-js
for spec in spec/*.ts; do
  name=$(basename "$spec" .ts)
  sed -e 's/init()\.then(() => done());//' "$spec" > "spec-js/$name.ts"
done
npx esbuild spec-js/*.ts --format=cjs --platform=node --outdir=spec-js --log-level=warning \
  --banner:js='const { describe, it, before, beforeEach, after, afterEach } = require("node:test");' 
# wasm-bindgen 0.2.45 named an object's pointer `ptr`; 0.2.129, `__wbg_ptr`.
perl -pi -e 's/\.ptr\b/.__wbg_ptr/g' spec-js/*.js
rm spec-js/*.ts
cp "$here/build.sh" build.sh
(cd rust && cargo generate-lockfile -q)
git add -A >/dev/null && git -c user.email=v@example.com -c user.name=v commit -qm "Specs run in Node"
