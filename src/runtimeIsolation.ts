const RUNTIME_INSTANCE_MARKER = "__SUPERCOV_RUNTIME_INSTANCE__";

/** Give only the generated collector runtime a private global-state key. */
export function isolateCollectorRuntime(
  source: string,
  collectorId: string,
): string {
  const assignment = new RegExp(
    `(runtimeInstanceToken\\s*=\\s*["'])${RUNTIME_INSTANCE_MARKER}(["'])`,
  );
  if (!assignment.test(source))
    throw new Error("Generated Supercov runtime is missing its instance marker");
  return source.replace(assignment, `$1${collectorId}$2`);
}
