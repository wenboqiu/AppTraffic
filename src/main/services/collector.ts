/**
 * Polls every traffic source once per tick, books their per-process deltas
 * into the day store and keeps the live speed / connection figures the UI shows.
 */

import { addDelta, aggregate, earliestDay, saveStore } from './store';
import { ClashSource } from './sources/clash';
import { EtwSource } from './sources/etw';
import { Reconciler } from './merge';
import { systemProxyPorts } from './sysproxy';
import type { EtwSample, TrafficSource } from './sources/types';
import { DIRECT } from '../../shared/types';
import type { ProcRow, Range, Snapshot } from '../../shared/types';

const POLL_MS = 1000;
const SAVE_EVERY_MS = 30_000;
const TOP_HOSTS = 8;

const clash = new ClashSource();
const etw = new EtwSource();
const sources: TrafficSource<unknown>[] = [clash, etw];
const reconciler = new Reconciler();
let liveConns = new Map<string, number>();
let lastTickAt = 0;
let speed = { up: 0, down: 0 };
let procSpeed = new Map<string, { up: number; down: number }>();
let timer: NodeJS.Timeout | null = null;
let saveTimer: NodeJS.Timeout | null = null;
let busy = false;
/** ETW samples held back while the proxy API is still being discovered. */
let heldEtw: EtwSample | null = null;

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

/** Run a source's button action (currently only "start the ETW helper"). */
export async function runSourceAction(id: string): Promise<void> {
  if (id === etw.id) await etw.start();
  listeners.forEach((fn) => fn());
}

async function settle<S>(src: TrafficSource<S>): Promise<S | null> {
  try {
    return await src.poll();
  } catch {
    src.reset();
    return null;
  }
}

async function tick(): Promise<void> {
  if (busy) return; // a slow request must not stack up overlapping polls
  busy = true;
  try {
    const [c, polled] = await Promise.all([settle(clash), settle(etw)]);
    const now = Date.now();
    // Until discovery says whether a Clash API exists, ETW can't tell a core's bytes
    // from an app's; hold them and reconcile together once it's known.
    let e = polled;
    if (e && clash.searching) {
      heldEtw = heldEtw ? { procs: [...heldEtw.procs, ...e.procs], loops: [...heldEtw.loops, ...e.loops] } : e;
      e = null;
    } else if (heldEtw) {
      e = { procs: [...heldEtw.procs, ...(e?.procs ?? [])], loops: [...heldEtw.loops, ...(e?.loops ?? [])] };
      heldEtw = null;
    }
    liveConns = c?.liveConns ?? new Map();

    if (!c && !e) {
      reconciler.reset();
      lastTickAt = 0;
      speed = { up: 0, down: 0 };
      procSpeed.clear();
      return;
    }

    const tickSpeed = new Map<string, { up: number; down: number }>();
    for (const d of reconciler.merge(c, e, systemProxyPorts())) {
      addDelta(d);
      const s = tickSpeed.get(d.proc) ?? { up: 0, down: 0 };
      s.up += d.up;
      s.down += d.down;
      tickSpeed.set(d.proc, s);
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

  const routeTotals = new Map<string, number>();
  const procs: ProcRow[] = Object.entries(agg)
    .map(([name, s]) => ({
      name,
      path: s.path,
      up: s.up,
      down: s.down,
      direct: s.routes[DIRECT] ?? 0,
      routes: Object.entries(s.routes)
        .filter(([r]) => r !== DIRECT)
        .sort((a, b) => b[1] - a[1]),
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

  for (const p of procs) for (const [r, b] of p.routes) routeTotals.set(r, (routeTotals.get(r) ?? 0) + b);

  return {
    range,
    sources: sources.map((s) => s.status()),
    totalUp: procs.reduce((a, p) => a + p.up, 0),
    totalDown: procs.reduce((a, p) => a + p.down, 0),
    totalDirect: procs.reduce((a, p) => a + p.direct, 0),
    routes: [...routeTotals].sort((a, b) => b[1] - a[1]),
    speedUp: speed.up,
    speedDown: speed.down,
    procs,
    since: earliestDay(),
  };
}
