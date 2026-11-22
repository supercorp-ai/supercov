// Diagnosis, loaded with --import into Vitest's main process: counts the bytes each text stream
// decodes, by where its decoder was made, and writes the five largest, with a sample of their
// text, to $DIAG_OUT/decoders.json every 50 MB.
import { StringDecoder } from "node:string_decoder";
import { Readable } from "node:stream";
import { writeFileSync } from "node:fs";

if (process.argv.some((arg) => /vitest\.mjs$/.test(arg))) {
    const out = `${process.env.DIAG_OUT ?? "/tmp"}/decoders-${process.pid}.json`;
    const totals = new Map();
    let all = 0;
    let next = 50e6;
    const where = () => new Error().stack.split("\n").slice(2, 16).join("\n");
    const setEncoding = Readable.prototype.setEncoding;
    Readable.prototype.setEncoding = function (encoding) {
        const result = setEncoding.call(this, encoding);
        const decoder = this._readableState?.decoder;
        if (decoder)
            decoder.supercovWhere = `setEncoding on ${this.constructor?.name}\n${where()}`;
        return result;
    };
    const write = StringDecoder.prototype.write;
    StringDecoder.prototype.write = function (buffer) {
        const text = write.call(this, buffer);
        this.supercovWhere ??= `first write\n${where()}`;
        const entry = totals.get(this.supercovWhere) ?? { bytes: 0, writes: 0, sample: "" };
        entry.bytes += buffer?.length ?? 0;
        entry.writes += 1;
        if (entry.sample.length < 2000)
            entry.sample += String(text).slice(0, 2000 - entry.sample.length);
        totals.set(this.supercovWhere, entry);
        all += buffer?.length ?? 0;
        if (all > next) {
            next += 50e6;
            const top = [...totals].sort((a, b) => b[1].bytes - a[1].bytes).slice(0, 5).map(([at, e]) => ({ at, ...e }));
            writeFileSync(out, JSON.stringify({ all, memory: process.memoryUsage(), top }, null, 1));
        }
        return text;
    };
}
