const pop = Array['prototype'].pop;
const makeMethod = () => pop;
const shared = { info: makeMethod() };

export function nativeSharedLogger() { return shared; }
