import util from 'node:util';
const identity = (args) => args;
const stdout = ({ format = identity } = { format: identity }) =>
  (...args) => console.log('prefix', ...format(args));
const stderr = ({ format = identity } = { format: identity }) =>
  (...args) => console.error('prefix', ...format(args));
const silent = { info: () => {}, error: () => {} };
const normal = { info: stdout(), error: stderr() };
const normalStderr = { info: stderr(), error: stderr() };
const formatData = (args) => args.map((arg) => {
  if (typeof arg === 'object') return util.inspect(arg, { depth: null, colors: process.stderr.isTTY, compact: false });
  return arg;
});
const verbose = { info: stdout({ format: formatData }), error: stderr({ format: formatData }) };
const verboseStderr = { info: stderr({ format: formatData }), error: stderr({ format: formatData }) };
export const selectLogger = ({ mode, destination }) => {
  if (mode === 'quiet') { return silent; }
  if (mode === 'verbose') { return destination === 'terminal' ? verboseStderr : verbose; }
  return destination === 'terminal' ? normalStderr : normal;
};
