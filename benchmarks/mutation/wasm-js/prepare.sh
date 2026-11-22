#!/bin/sh
# Rust compiled to wasm under JavaScript tests, for validation against
# cargo-mutants: gl-matrix-wasm (a port of gl-matrix to Rust with
# wasm-bindgen), whose own Mocha specs test the wasm.
#
#   sh prepare.sh DIR
#
# Clones it into DIR at a fixed commit and makes its specs run in Node, as
# they ran in Chrome under Karma: the crate built for wasm32 and bound with
# `wasm-bindgen --target nodejs` (the specs' `init()`, the web glue's, is
# not needed there), the TypeScript specs transpiled to CommonJS with
# esbuild, and wasm-bindgen moved to the CLI's version without the nightly
# feature. `npm test` is the test command both tools run: the wasm built
# (build.sh) and vec2's spec run against it.
set -e
dir=$1
here=$(cd "$(dirname "$0")" && pwd)
git clone -q https://github.com/dtysky/gl-matrix-wasm "$dir"
cd "$dir"
git checkout -q e84418b547693d69893188818e6e71f41f8e0bf8
sed -i.orig -e 's/^version = "0.2.45"/version = "=0.2.129"/' -e '/^features = \[/,/^\]/d' Cargo.toml
rm -f Cargo.toml.orig Cargo.lock
cat > package.json <<'JSON'
{ "name": "gl-matrix-wasm-specs", "private": true, "scripts": { "test": "sh build.sh && mocha --reporter dot spec-js/vec2-spec.cjs" }, "devDependencies": { "esbuild": "0.25.10", "mocha": "11.7.4" } }
JSON
npm install --no-audit --no-fund --loglevel error
mkdir -p spec-js
for spec in spec/*.ts; do
  name=$(basename "$spec" .ts)
  sed -e 's/init()\.then(() => done())/done()/' "$spec" > "spec-js/$name.ts"
done
npx esbuild spec-js/*.ts --format=cjs --platform=node --outdir=spec-js --out-extension:.js=.cjs --log-level=warning
rm spec-js/*.ts
cp "$here/build.sh" build.sh
cargo generate-lockfile -q
git add -A >/dev/null && git -c user.email=v@example.com -c user.name=v commit -qm "Specs run in Node"
