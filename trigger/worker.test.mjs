// Checks the trigger rule. Run with `node trigger/worker.test.mjs` (or JavaScriptCore: `jsc -m`).
import {lastSettled, decide} from "./worker.js";
const say = globalThis.print ?? console.log;
const eq = (got, want, what) => { if (got !== want) throw new Error(`${what}: got ${got}, want ${want}`); say(`ok ${what}`); };
const at = (s) => lastSettled(new Date(s)).toISOString();
eq(at("2026-11-09T22:37:00Z"), "2026-11-09T21:30:00.000Z", "monday evening");
eq(at("2026-11-10T08:00:00Z"), "2026-11-09T21:30:00.000Z", "tuesday before close");
eq(at("2026-11-08T18:00:00Z"), "2026-11-06T21:30:00.000Z", "sunday");
eq(at("2026-07-01T20:30:00Z"), "2026-07-01T20:30:00.000Z", "summer cutoff exact");
eq(at("2026-07-01T20:29:00Z"), "2026-06-30T20:30:00.000Z", "summer just before");
eq(at("2026-11-02T03:00:00Z"), "2026-10-30T20:30:00.000Z", "after fall-back weekend");
eq(decide([]), "start", "no runs");
eq(decide([{status: "in_progress"}]), "up_to_date", "running");
eq(decide([{status: "completed", conclusion: "success"}]), "up_to_date", "succeeded");
eq(decide([{status: "completed", conclusion: "failure"}]), "start", "one failure");
eq(decide(Array(3).fill({status: "completed", conclusion: "failure"})), "gave_up", "three failures");
