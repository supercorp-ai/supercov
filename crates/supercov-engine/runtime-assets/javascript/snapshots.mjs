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
    // What each comparison and infection probe saw: a page's probes run in
    // the browser, and a merge that left them out lost every outcome there.
    const comparisons = new Map();
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
        for (const comparison of snapshot?.comparisons ?? []) {
            let outcomes = comparisons.get(comparison.id);
            if (!outcomes) {
                outcomes = new Set();
                comparisons.set(comparison.id, outcomes);
            }
            for (const outcome of comparison.outcomes ?? [])
                outcomes.add(outcome);
        }
    }
    const merged = {
        decisions: [...decisions.values()].map(({ seen, id }) => ({ id, vectors: [...seen.values()] })),
        hits: [...hits],
        events: [...events.values()],
        logicals: [...logicals.values()].map(({ seen, id }) => ({ id, vectors: [...seen.values()] })),
    };
    if (comparisons.size)
        merged.comparisons = [...comparisons].map(([id, outcomes]) => ({ id, outcomes: [...outcomes] }));
    return merged;
}
/**
 * A snapshot as JSON.parse would give it from the text a page's runtime
 * wrote compactly (runtime.mjs, compactSnapshotText): each event in its own
 * key order, its strings from the shared table. Anything else is returned
 * as it is.
 */
export function expandSnapshot(value) {
    if (!value || typeof value !== "object" || value.supercovCompactSnapshot !== 1)
        return value;
    const { order, rest, strings, shapes, rows, hits } = value;
    const events = [];
    for (let position = 0; position < rows.length;) {
        const fields = shapes[rows[position++]];
        const event = {};
        for (const field of fields) {
            const raw = rows[position++];
            event[field.slice(1)] = field[0] === "s" ? strings[raw] : raw;
        }
        events.push(event);
    }
    const snapshot = {};
    for (const key of order)
        snapshot[key] = key === "events" ? events : key === "hits" ? hits.map((index) => strings[index]) : rest[key];
    return snapshot;
}
/**
 * A snapshot's events as columns: each event's type, phase and environment
 * as an index into a table of the distinct ones, its time as the difference
 * from the event before it. The reader gives back the same events.
 */
export function columnarEvents(snapshot) {
    const events = snapshot?.events;
    if (!Array.isArray(events) || events.length === 0)
        return snapshot;
    const tables = { types: new Map(), phases: new Map(), environments: new Map() };
    const index = (table, value) => {
        let found = table.get(value);
        if (found === undefined) {
            found = table.size;
            table.set(value, found);
        }
        return found;
    };
    const columns = { id: [], type: [], phase: [], environment: [], t: [], vector: {} };
    let previous = 0;
    events.forEach((event, position) => {
        columns.id.push(event.id);
        columns.type.push(index(tables.types, event.type));
        columns.phase.push(event.phaseId == null ? -1 : index(tables.phases, event.phaseId));
        columns.environment.push(index(tables.environments, event.environment));
        columns.t.push(event.timestampMs - previous);
        previous = event.timestampMs;
        if (event.vector !== undefined)
            columns.vector[position] = event.vector;
    });
    const { events: _events, ...rest } = snapshot;
    return {
        ...rest,
        eventColumns: {
            types: [...tables.types.keys()],
            phases: [...tables.phases.keys()],
            environments: [...tables.environments.keys()],
            ...columns,
        },
    };
}
