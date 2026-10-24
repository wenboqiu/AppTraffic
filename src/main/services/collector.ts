/**
 * Polls every traffic source once per tick, books their per-process deltas
 * into the day store and keeps the live speed / connection figures the UI shows.
 */

import { addDelta, aggregate, earliestDay, saveStore } from './store';
import { ClashSource } from './sources/clash';
import type { TrafficSource } from './sources/types';
import type { ProcRow, Range, Snapshot } from '../../shared/types';

const POLL_MS = 1000;
const SAVE_EVERY_MS = 30_000;
const TOP_HOSTS = 8;

const sources: TrafficSource[] = [new ClashSource()];
let liveConns = new Map<string, number>();
let lastTickAt = 0;
let speed = { up: 0, down: 0 };
let procSpeed = new Map<string, { up: number; down: number }>();
let timer: NodeJS.Timeout | null = null;
let saveTimer: NodeJS.Timeout | null = null;
let busy = false;

type TickListener = () => void;
const listeners: TickListener[] = [];
export function onTick(fn: TickListener): void {
  listeners.push(fn);
}

export function startCollector(): void {
  if (timer) return;
  timer = setInterval(tick, POLL_MS);
  saveTimer = setInterval(() => saveStore(), SAVE_EVERY_MS);
  tick();
}

export function stopCollector(): void {
  if (timer) clearInterval(timer);
  if (saveTimer) clearInterval(saveTimer);
  timer = saveTimer = null;
  saveStore(true);
}

async function tick(): Promise<void> {
  if (busy) return; // a slow request must not stack up overlapping polls
  busy = true;
  try {
    const results = await Promise.allSettled(sources.map((s) => s.poll()));
    const now = Date.now();

    const tickSpeed = new Map<string, { up: number; down: number }>();
    const conns = new Map<string, number>();
    let anyOk = false;
    results.forEach((r, i) => {
      if (r.status === 'rejected') {
        sources[i].reset();
        return;
      }
      anyOk = true;
      for (const d of r.value.deltas) {
        addDelta(d);
        const s = tickSpeed.get(d.proc) ?? { up: 0, down: 0 };
        s.up += d.up;
        s.down += d.down;
        tickSpeed.set(d.proc, s);
      }
      for (const [proc, n] of r.value.liveConns) conns.set(proc, (conns.get(proc) ?? 0) + n);
    });
    liveConns = conns;

    if (!anyOk) {
      lastTickAt = 0;
      speed = { up: 0, down: 0 };
      procSpeed.clear();
      return;
    }
    // The first tick after (re)connecting only sets baselines, so there's no interval to divide by yet
    if (lastTickAt) {
      const secs = Math.max((now - lastTickAt) / 1000, 0.001);
      let up = 0;
      let down = 0;
      for (const s of tickSpeed.values()) {
        up += s.up;
        down += s.down;
      }
      speed = { up: up / secs, down: down / secs };
      procSpeed = new Map([...tickSpeed].map(([k, s]) => [k, { up: s.up / secs, down: s.down / secs }]));
    }
    lastTickAt = now;
  } finally {
    busy = false;
    listeners.forEach((fn) => fn());
  }
}

export function getSnapshot(range: Range): Snapshot {
  const agg = aggregate(range);

  const procs: ProcRow[] = Object.entries(agg)
    .map(([name, s]) => ({
      name,
      path: s.path,
      up: s.up,
      down: s.down,
      direct: s.direct,
      conns: s.conns,
      liveConns: liveConns.get(name) ?? 0,
      speedUp: procSpeed.get(name)?.up ?? 0,
      speedDown: procSpeed.get(name)?.down ?? 0,
      topHosts: Object.entries(s.hosts)
        .sort((a, b) => b[1] - a[1])
        .slice(0, TOP_HOSTS),
    }))
    .filter((p) => p.up + p.down > 0 || p.liveConns > 0)
    .sort((a, b) => b.up + b.down - (a.up + a.down));

  return {
    range,
    sources: sources.map((s) => s.status()),
    totalUp: procs.reduce((a, p) => a + p.up, 0),
    totalDown: procs.reduce((a, p) => a + p.down, 0),
    totalDirect: procs.reduce((a, p) => a + p.direct, 0),
    speedUp: speed.up,
    speedDown: speed.down,
    procs,
    since: earliestDay(),
  };
}
