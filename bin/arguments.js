// Yarn 1 runs `yarn supercov --exact-attribution -- npm test` as `supercov
// npm test`: it takes the options before `--` for its own and drops the
// `--`. The binary would measure without the option and never know. Yarn
// keeps what was typed in `npm_config_argv`, and it is used when it is this
// executable's command line: one Yarn started for `supercov`, of which what
// arrived is the words in order with some taken out. A variable inherited
// from an outer run names other words and is left alone. (A `--` lost on its
// own the binary recovers itself: it reads a first word that names a
// program as the test command.)
export function argumentsAsTyped(received, environment) {
  if (!/^yarn\/1\./.test(environment.npm_config_user_agent ?? "")) return received;
  let original;
  try {
    original = JSON.parse(environment.npm_config_argv ?? "").original;
  } catch {
    return received;
  }
  if (!Array.isArray(original) || !original.every((word) => typeof word === "string"))
    return received;
  const start = original[0] === "run" || original[0] === "exec" ? 1 : 0;
  if (original[start] !== "supercov") return received;
  const typed = original.slice(start + 1);
  let matched = 0;
  for (const word of typed) if (matched < received.length && word === received[matched]) matched += 1;
  return matched === received.length && typed.length > received.length ? typed : received;
}
