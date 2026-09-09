export function selfOnly() {
  return 'self';
}
export function aliasOnly() {
  return 'alias';
}
export function independent() {
  return 'fixed';
}
export function overwritten() {
  return 'reset';
}
let calls = 0;
export function successive() {
  calls++;
  return 'successive';
}
export function withGetter() {
  let reads = 0;
  return { get value() { reads++; return 'getter'; } };
}
export function strictSelf() {
  return 17;
}
export function strictAlias() {
  return 23;
}
export function independentlyChecked() {
  return 'checked';
}
export function awaitedSelf() {
  return 'awaited';
}
export function looseSelf() {
  return 'loose';
}
export function looseDeepSelf() {
  return 'loose-deep';
}
export function explicitStrictSelf() {
  return 'explicit-strict';
}
export function shadowed() {
  return 'shadowed';
}
export function typedSelf() {
  return 'typed';
}
