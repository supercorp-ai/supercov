const identity = (args) => args;
const stdout = ({ format = identity } = { format: identity }) =>
  (...args) => console.log('prefix', ...format(args));
const stderr = () => (...args) => console.error('prefix', ...args);
const logger = { info: stdout(), error: stderr() };
export const getLogger = () => logger;
