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
export function awaitedReverse() { return 'reverse'; }
export function awaitedAlias() { return 'awaited alias'; }
let thenableReads = 0;
export function awaitedTwice() { return 'twice awaited'; }
export function awaitedIndependent() { return 'independent awaited'; }
export function awaitedChecked() { return 'checked awaited'; }
export function awaitedMutable() { return 'mutable awaited'; }
let awaitedCallsCount = 0;
export function awaitedCalls() { return 'calls awaited'; }
export function awaitedGetter() {
  let reads = 0;
  return { get value() { return 'getter awaited'; } };
}
export function awaitedTyped() { return 'typed awaited'; }
