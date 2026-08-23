import { describe, expect, it } from "vitest";
import { transformCapabilityImports } from "../../src/esmInterceptor";

describe("pure ESM capability interception", () => {
  it("wraps imported values in a provider-neutral remote launcher", () => {
    const result = transformCapabilityImports(
      `import { ImageBuilder } from './sdk.mjs';\nawait ImageBuilder.build({ mounts: [{ source: process.cwd(), target: '/workspace' }], snapshotKey: 'base' });`,
      "file:///project/runner.mjs",
      "file:///project/.supercov/launchSupervisor.js",
    );
    expect(result.transformed).toBe(true);
    expect(result.code).toContain("wrapImportedCapability");
    expect(result.code).toContain("ImageBuilderSupercovRaw");
  });

  it("leaves ordinary ESM imports byte-for-byte untouched", () => {
    const source = `import { format } from './format.mjs';\nconsole.log(format('value'));`;
    expect(
      transformCapabilityImports(source, "file:///project/main.mjs", "file:///wrapper.mjs"),
    ).toEqual({ code: source, transformed: false });
  });
});
