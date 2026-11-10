// Stryker disable comments: next-line and whole-file scopes, mutator lists,
// reasons, restores, and comments StrykerJS reads as something else.
import { helper } from "./helper.js";
import {
  // Stryker disable all
  other,
} from "./other.js";

// Stryker disable next-line StringLiteral
const first = "ignored" + helper("kept");
const second = "kept"; // Stryker disable next-line all
const third = 1 + 2;
const fourth = // Stryker disable next-line ArithmeticOperator
  1 + 2 + "s".length;
// Stryker disable next-line stringliteral,BOOLEANLITERAL: case does not matter
const fifth = ["x", true];
// Stryker disable next-line all because the list ends at a colon
const sixth = ["still", "mutated"];
/* Stryker disable next-line ArrayDeclaration */ const seventh = [1];
/** Stryker disable next-line all */
const eighth = [2];
//Stryker disable next-line all
const ninth = [3];
//  Stryker disable next-line all
const tenth = [4];
// stryker disable next-line all
const eleventh = [5];
// Stryker disable next-line Nonexistent
const twelfth = [6];

/* Stryker disable BooleanLiteral */
export const flags = [true, !first];
/* Stryker restore all */
export const restored = false;

function trailing() {
  helper();
  // Stryker disable all
}
export const afterTrailing = 1 - 1;

// Stryker disable ConditionalExpression,EqualityOperator: one reason
if (first > second) {
  helper();
}
// Stryker restore ConditionalExpression
if (first > second) {
  helper();
}
// Stryker restore all

// Stryker disable next-line all
if (third) {
  helper("next line only covers nodes starting on it");
}

export function calls() {
  // Stryker disable next-line StringLiteral
  helper("only mutant is disabled");
  // Stryker disable next-line Regex
  helper(/^only regex$/);
  helper(/^regex$/);
  // Stryker disable next-line CallExpression
  helper();
}

// Stryker disable next-line all
export default function exported() {
  return [trailing, fourth, fifth, sixth, seventh, eighth, ninth, tenth, eleventh, twelfth];
}

export function View({ items }) {
  return (
    <ul className={other}>
      {/* Stryker disable next-line all */}
      {items.map((item) => <li key={item}>{item || "none"}</li>)}
    </ul>
  );
}

// Stryker disable all
export const everything = { off: "yes" };
// Stryker restore StringLiteral
export const partly = { on: "yes" };
