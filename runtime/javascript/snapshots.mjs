/**
 * Two events JSON.stringify would write alike: the same keys in the same
 * order, with the same values. Comparing is several times cheaper than
 * writing both, and a test's pages hand over thousands of events each time
 * one of them leaves.
 */
function sameEvent(left, right) {
    if (left === right)
        return true;
    if (left === null || right === null || typeof left !== "object" || typeof right !== "object")
        return false;
    const leftKeys = Object.keys(left);
    const rightKeys = Object.keys(right);
    if (leftKeys.length !== rightKeys.length)
        return false;
    for (let index = 0; index < leftKeys.length; index += 1) {
        const key = leftKeys[index];
        if (key !== rightKeys[index])
            return false;
        const leftValue = left[key];
        const rightValue = right[key];
        if (leftValue === rightValue)
            continue;
        if (leftValue === null || rightValue === null || typeof leftValue !== "object" || typeof rightValue !== "object" ||
            JSON.stringify(leftValue) !== JSON.stringify(rightValue))
            return false;
    }
    return true;
}
/**
 * One snapshot holding everything in `snapshots`, each observation once: a
 * document that left gave its evidence, and the next one on its origin
 * restored the same evidence and added to it.
 */
export function mergeSnapshots(snapshots) {
    const hits = new Set();
    const events = new Map();
    const decisions = new Map();
    const logicals = new Map();
    const vectorsOf = (map, key, initial) => {
        let entry = map.get(key);
        if (!entry) {
            entry = { ...initial, seen: new Map() };
            map.set(key, entry);
        }
        return entry;
    };
    let previousEvents;
    for (const snapshot of snapshots) {
        for (const id of snapshot?.hits ?? [])
            hits.add(id);
        // A document restores the evidence the one before it on its origin
        // left, in order, before adding its own: the events it repeats from
        // the snapshot before are in `events` already, and are compared
        // instead of serialized.
        const list = snapshot?.events ?? [];
        let start = 0;
        if (previousEvents !== undefined) {
            const shared = Math.min(previousEvents.length, list.length);
            while (start < shared && sameEvent(previousEvents[start], list[start]))
                start += 1;
        }
        for (let index = start; index < list.length; index += 1)
            events.set(JSON.stringify(list[index]), list[index]);
        previousEvents = list;
        for (const decision of snapshot?.decisions ?? []) {
            const entry = vectorsOf(decisions, decision.id, { id: decision.id });
            for (const vector of decision.vectors ?? [])
                entry.seen.set(JSON.stringify(vector), vector);
        }
        for (const logical of snapshot?.logicals ?? []) {
            const entry = vectorsOf(logicals, logical.id, { id: logical.id });
            for (const vector of logical.vectors ?? [])
                entry.seen.set(JSON.stringify(vector), vector);
        }
    }
    return {
        decisions: [...decisions.values()].map(({ seen, id }) => ({ id, vectors: [...seen.values()] })),
        hits: [...hits],
        events: [...events.values()],
        logicals: [...logicals.values()].map(({ seen, id }) => ({ id, vectors: [...seen.values()] })),
    };
}
