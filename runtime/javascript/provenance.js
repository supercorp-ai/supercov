const NORMALIZED_KINDS = [
    ["unit", /(^|[/_.-])unit([/_.-]|$)/i],
    ["component", /(^|[/_.-])(component|components|ct)([/_.-]|$)/i],
    ["integration", /(^|[/_.-])(integration|int)([/_.-]|$)/i],
    ["e2e", /(^|[/_.-])(e2e|end-to-end|offline|online)([/_.-]|$)/i],
];
function classifiedKind(value) {
    if (!value)
        return undefined;
    return NORMALIZED_KINDS.find(([, pattern]) => pattern.test(value))?.[0];
}
export function inferTestProvenance({ runner, file, project, explicitKind, }) {
    if (explicitKind?.trim()) {
        return {
            runner,
            kind: explicitKind.trim().toLowerCase(),
            ...(project ? { project } : {}),
            source: "explicit",
        };
    }
    const projectKind = classifiedKind(project);
    if (projectKind) {
        return {
            runner,
            kind: projectKind,
            ...(project ? { project } : {}),
            source: "project",
        };
    }
    const pathKind = classifiedKind(file);
    if (pathKind) {
        return {
            runner,
            kind: pathKind,
            ...(project ? { project } : {}),
            source: "path",
        };
    }
    const defaultKind = runner === "playwright"
        ? "e2e"
        : runner === "vitest" || runner === "jest" || runner === "node:test"
            ? "unit"
            : "unknown";
    return {
        runner,
        kind: defaultKind,
        ...(project ? { project } : {}),
        source: defaultKind === "unknown" ? "unknown" : "runner-default",
    };
}
