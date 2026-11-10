// Strings, templates and JSX, with text outside ASCII before mutants so
// columns count UTF-16 code units.
import React, { useState } from "react";

type Props = { title: string; items: string[]; tone?: "loud" | "quiet" };

const greeting = "héllo wörld";
const emoji = "🎉 party"; const after = "after the emoji";
const astral = `𝒳${greeting}𝒴`; const flag = !astral;
const empty = "";
const blankTemplate = ``;
const spaced = ` `;
const joined = `${greeting} and ${emoji}`;
const tagged = String.raw`raw\n${greeting}`;
const keys = { "quoted": 1, plain: "value", [`computed`]: 2, 'single': 'quotes' };
const { "quoted": renamed = "fallback" } = keys;
const multiline = `first line
second line ${greeting.length > 3 ? "long" : "short"}`;
const lineSeparator = "a b"; const afterSeparator = "c";

export function Card({ title, items, tone = "quiet" }: Props) {
  const [open, setOpen] = useState<boolean>(false);
  const className = `card card--${tone}`;
  return (
    <section className="card" data-tone={tone} aria-label={"Card: " + title} title={`t`}>
      <h2 style={{ fontWeight: open ? 700 : 400 }}>{title || "Untitled"}</h2>
      {items.length > 0 && (
        <ul>
          {items.map((item) => (
            <li key={item} onClick={() => setOpen(!open)}>
              {item.trim()} — ✓
            </li>
          ))}
        </ul>
      )}
      {items.length === 0 ? <p>Nothing here</p> : null}
      <Generic<string> value="v" render={(v) => <span>{v}</span>} />
      <>
        <input disabled={!open} placeholder="type…" />
        {/* a comment inside JSX */}
      </>
      <p className={className}>{tone === "loud" ? "!" : "."}</p>
    </section>
  );
}

function Generic<T>({ value, render }: { value: T; render: (v: T) => React.ReactNode }) {
  return <div>{render(value)}</div>;
}
