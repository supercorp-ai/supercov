import { parse, type ParserPlugin } from "@babel/parser";
import { describe, expect, it } from "vitest";
import { instrumentMcdc } from "../../src/instrumenter";

const syntaxCases: Array<{
  file: string;
  plugins: ParserPlugin[];
  source: string;
}> = [
  {
    file: "app/modern.js",
    plugins: [],
    source: `
      export class State {
        static #instances = 0;
        static { this.#instances ||= 1; }
        value = 0;
        update(input) { this.value ??= input?.value; return this.value && input.valid; }
      }
      export async function load(value) {
        await using resource = value;
        return resource?.ready && !resource.closed;
      }
    `,
  },
  {
    file: "app/view.jsx",
    plugins: ["jsx"],
    source: `
      export function View({ ready, fallback }) {
        return <main>{ready && fallback ? <span>ready</span> : null}</main>;
      }
    `,
  },
  {
    file: "app/model.ts",
    plugins: ["typescript", "decorators-legacy"],
    source: `
      @sealed
      export class Model<T extends { valid: boolean }> {
        constructor(public value: T) {}
        result(): boolean { return this.value.valid && Boolean(this.value); }
      }
      export const config = { enabled: true } satisfies Record<string, boolean>;
    `,
  },
  {
    file: "app/component.tsx",
    plugins: ["typescript", "decorators-legacy", "jsx"],
    source: `
      type Props = { enabled?: boolean; label: string };
      export const Component = ({ enabled = true, label }: Props) =>
        <button disabled={!enabled || label.length === 0}>{label}</button>;
    `,
  },
];

describe("Babel syntax compatibility", () => {
  for (const fixture of syntaxCases) {
    it(`round-trips ${fixture.file}`, () => {
      const result = instrumentMcdc(fixture.source, fixture.file);
      expect(result.code).toContain("virtual:supercov-runtime");
      expect(() =>
        parse(result.code, {
          sourceType: "unambiguous",
          plugins: fixture.plugins,
        }),
      ).not.toThrow();
    });
  }
});
