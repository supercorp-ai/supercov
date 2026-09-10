const identity = (args) => args;
const log = ({ prefix = 'factory', format = identity } = { prefix: 'factory', format: identity }) =>
  (...args) => console.log(prefix, ...format(args));
const quiet = { info: () => {} };
const moduleLog = log({ prefix: 'module closure', format: identity });

export function createLogger({ enabled, stream }) {
  if (enabled === false) return { info: () => {} };
  return stream === 'error'
    ? { info: (...args) => console.error(...args) }
    : { info: log() };
}
export function createTagged(prefix) { return { info: log({ prefix, format: identity }) }; }
export function sharedLogger() { return quiet; }
export function fromModuleClosure() { moduleLog('module closure payload'); }
export function sideEffectArgs() {
  return { info: () => console.log(console.log('nested call')) };
}
export function thisLogger() { return { info: function () { console.log(this); } }; }
export function spreadLogger() { return { info: (args) => console.log(...args) }; }
export function inheritedLogger(options) { return { info: log(options) }; }
export function conditionalClosure(enabled) { return () => { if (enabled) console.log('captured branch'); }; }
