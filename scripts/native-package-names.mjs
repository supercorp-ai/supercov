import assert from "node:assert/strict";

export function nativePackageStem(packageName) {
  assert.match(
    packageName,
    /^(?:@[a-z0-9][a-z0-9._-]*\/)?[a-z0-9][a-z0-9._-]*$/,
    `invalid npm package name: ${packageName}`,
  );
  return packageName.startsWith("@")
    ? packageName.slice(1).replace("/", "-")
    : packageName;
}

export function nativeTarballName(packageName, version) {
  return `${nativePackageStem(packageName)}-${version}.tgz`;
}

export function nativeChecksumName(packageName) {
  return `${nativePackageStem(packageName)}.checksums.json`;
}

// The registry lists every target the matrix can build; the release set is the
// subset the primary package depends on. A target held out with
// `"publish": false` is built and validated like the others but never enters
// optionalDependencies or the published set, which is how a platform is proven
// on its runner before a release is asked to depend on it.
export function releaseTargets(registry) {
  return registry.targets.filter((target) => target.publish !== false);
}
