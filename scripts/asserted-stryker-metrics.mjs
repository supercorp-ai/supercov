// Replay reporting only. Unknowns never leave the frozen comparison cohort.
export function metrics(rows, field) {
  const result = {
    total: rows.length,
    tp: 0,
    fp: 0,
    fn: 0,
    tn: 0,
    unknownKilled: 0,
    unknownSurvived: 0,
  };
  for (const row of rows) {
    const prediction = row[field];
    if (prediction === true) result[row.actualKilled ? "tp" : "fp"]++;
    else if (prediction === false) result[row.actualKilled ? "fn" : "tn"]++;
    else result[row.actualKilled ? "unknownKilled" : "unknownSurvived"]++;
  }
  const divide = (a, b) => (b ? a / b : null);
  const answered = result.tp + result.fp + result.fn + result.tn;
  return {
    ...result,
    answered,
    unknown: result.total - answered,
    missedKillsIncludingUnknown: result.fn + result.unknownKilled,
    correct: result.tp + result.tn,
    fixedCohortAgreement: divide(result.tp + result.tn, result.total),
    answeredFraction: divide(answered, result.total),
    agreementAmongAnswers: divide(result.tp + result.tn, answered),
    killPrecision: divide(result.tp, result.tp + result.fp),
    killRecallIncludingUnknown: divide(
      result.tp,
      result.tp + result.fn + result.unknownKilled,
    ),
  };
}

/** An availability view in addition to, never instead of, the original binary
 * scoring policy. Positive predictions remain predictions, not proofs. A
 * negative involving a reported limit becomes an unknown. Whole-removal
 * predictions can depend on contained sites as well as the selected site.
 */
export function availabilityPrediction(binary, relevantResolutions) {
  if (binary !== false) return binary;
  return relevantResolutions.some((r) => r?.reason?.kind?.startsWith("limit:"))
    ? undefined
    : false;
}
