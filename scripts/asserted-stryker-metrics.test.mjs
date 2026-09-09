import test from "node:test";
import assert from "node:assert/strict";
import {
  metrics,
  availabilityPrediction,
} from "./asserted-stryker-metrics.mjs";

test("unknowns remain in the fixed denominator and recall loss", () => {
  const rows = [
    { actualKilled: true, p: true },
    { actualKilled: false, p: true },
    { actualKilled: true, p: false },
    { actualKilled: false, p: false },
    { actualKilled: true },
    { actualKilled: false },
  ];
  const m = metrics(rows, "p");
  assert.equal(m.total, 6);
  assert.equal(m.answered, 4);
  assert.equal(m.correct, 2);
  assert.equal(m.unknownKilled, 1);
  assert.equal(m.unknownSurvived, 1);
  assert.equal(m.fixedCohortAgreement, 2 / 6);
  assert.equal(m.agreementAmongAnswers, 2 / 4);
  assert.equal(m.killRecallIncludingUnknown, 1 / 3);
  assert.equal(m.missedKillsIncludingUnknown, 2);
});
test("abstaining on everything cannot score perfect accuracy", () => {
  const m = metrics([{ actualKilled: true }, { actualKilled: false }], "p");
  assert.equal(m.fixedCohortAgreement, 0);
  assert.equal(m.agreementAmongAnswers, null);
  assert.equal(m.killPrecision, null);
  assert.equal(m.killRecallIncludingUnknown, 0);
});
test("reported limits distinguish unknown negatives without erasing positive witnesses", () => {
  const limits = [{ reason: { kind: "limit:assertion-witness" } }];
  assert.equal(availabilityPrediction(false, limits), undefined);
  assert.equal(availabilityPrediction(true, limits), true);
  assert.equal(availabilityPrediction(undefined, []), undefined);
  assert.equal(
    availabilityPrediction(false, [{ reason: { kind: "gap:not-reached" } }]),
    false,
  );
});
