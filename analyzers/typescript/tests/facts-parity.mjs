import assert from "node:assert/strict";
import { isDeepStrictEqual } from "node:util";

function differences(actual, expected, path = "$", found = []) {
  if (found.length >= 12 || isDeepStrictEqual(actual, expected)) return found;
  if (
    actual &&
    expected &&
    typeof actual === "object" &&
    typeof expected === "object" &&
    Array.isArray(actual) === Array.isArray(expected)
  ) {
    for (const key of new Set([
      ...Object.keys(expected),
      ...Object.keys(actual),
    ]))
      differences(actual[key], expected[key], `${path}.${key}`, found);
  } else {
    const brief = (value) => JSON.stringify(value)?.slice(0, 180);
    found.push(`${path}: actual ${brief(actual)}; expected ${brief(expected)}`);
  }
  return found;
}

/** Compare JSON facts, ignoring only key order/root presentation and the two documented enrichments.
 * Array order, test ids, observations, boundaries, and all existing flow facts remain exact.
 * The reference resolutions are used ONLY here, never by the analyzer.
 */
export function assertFactsParity(actual, expected, resolutions) {
  actual = JSON.parse(JSON.stringify(actual));
  expected = JSON.parse(JSON.stringify(expected));
  delete actual.root;
  delete expected.root;
  const verdicts = new Map(resolutions.resolutions.map((r) => [r.site, r]));
  const sites = new Map(expected.sites.map((s) => [s.id, s]));
  let additionalEarlyExits = 0;
  let conditionalTimerCandidates = 0;
  for (const site of actual.sites) {
    const reference = sites.get(site.id);
    assert.ok(reference, `unexpected site ${site.id}`);
    if (
      site.decision &&
      !Object.hasOwn(reference.decision ?? {}, "earlyExitDownstream") &&
      Object.hasOwn(site.decision, "earlyExitDownstream")
    ) {
      assert.ok(
        site.decision.then,
        `early exit must have a branch: ${site.id}`,
      );
      assert.ok(
        site.decision.outcomes,
        `early exit must have per-test outcomes: ${site.id}`,
      );
      assert.ok(
        site.decision.earlyExitDownstream.every((id) => sites.has(id)),
        `unknown downstream site: ${site.id}`,
      );
      assert.ok(
        reference.decision.then.some((id) => {
          const verdict = verdicts.get(id);
          return (
            ["value", "total"].includes(verdict?.strength) &&
            verdict.tests?.some((test) =>
              reference.decision.outcomes.true.includes(test),
            )
          );
        }),
        `only an already-strong branch can omit an early-exit candidate: ${site.id}`,
      );
      delete site.decision.earlyExitDownstream;
      additionalEarlyExits++;
    }
    if (site.derive) {
      site.derive = site.derive.filter((dep) => {
        if (!Object.hasOwn(dep, "requiresTotal")) return true;
        assert.equal(site.category, "schedule");
        assert.equal(dep.label, "cancelled timer with total sink");
        assert.equal(dep.strength, "value");
        assert.ok(
          dep.requiresTotal.every((id) => sites.has(id)),
          `unknown timer callback site: ${site.id}`,
        );
        const active = dep.requiresTotal.some(
          (id) => verdicts.get(id)?.strength === "total",
        );
        conditionalTimerCandidates++;
        delete dep.requiresTotal;
        return active;
      });
      if (!site.derive.length) delete site.derive;
    }
  }
  assert.ok(
    isDeepStrictEqual(actual, expected),
    differences(actual, expected).join("\n"),
  );
  return { additionalEarlyExits, conditionalTimerCandidates };
}
