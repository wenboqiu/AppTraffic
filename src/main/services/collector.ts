/**
 * Polls mihomo's /connections once per tick and turns per-connection byte
 * counters into per-process deltas. Connections are short-lived and vanish
 * from the list once closed, so we remember each id's last reading and only
 * ever add the difference.
 */

import { MihomoClient } from './mihomo';
import { addDelta, aggregate, earliestDay, saveStore } from './store';
import { CORE_INNER, UNATTRIBUTED, UNKNOWN_PROCESS } from '../../shared/types';
import type { CoreStatus, ProcRow, Range, Snapshot } from '../../shared/types';

const POLL_MS = 1000;
const SAVE_EVERY_MS = 30_000;
const TOP_HOSTS = 8;

interface LiveConn {
  proc: string;
  up: number;
  down: number;
}

const client = new MihomoClient();
const live = new Map<string, LiveConn>();
let lastGlobal: { up: number; down: number } | null = null;
let lastTickAt = 0;
let speed = { up: 0, down: 0 };
let procSpeed = new Map<string, { up: number; down: number }>();
let status: CoreStatus = { connected: false, endpoint: '', version: null, error: '正在连接 Clash…' };
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
    const resp = await client.getConnections();
    const now = Date.now();
    const secs = lastTickAt ? Math.max((now - lastTickAt) / 1000, 0.001) : 0;
    lastTickAt = now;

    const seen = new Set<string>();
    const tickSpeed = new Map<string, { up: number; down: number }>();
    let attributedUp = 0;
    let attributedDown = 0;
    // First reading after start or reconnect: connections already open carry bytes
    // from before (possibly counted by a previous run), so only record a baseline.
    const baseline = lastGlobal === null;

    for (const c of resp.connections ?? []) {
      seen.add(c.id);
      // "Inner" connections are ones the core opens itself (DNS upstreams, rule-provider updates…)
      const proc = c.metadata.process || (c.metadata.type === 'Inner' ? CORE_INNER : UNKNOWN_PROCESS);
      if (baseline) {
        live.set(c.id, { proc, up: c.upload, down: c.download });
        continue;
      }
      const prev = live.get(c.id);
      const dUp = Math.max(0, c.upload - (prev?.up ?? 0));
      const dDown = Math.max(0, c.download - (prev?.down ?? 0));
      live.set(c.id, { proc, up: c.upload, down: c.download });

      attributedUp += dUp;
      attributedDown += dDown;
      if (!prev || dUp || dDown) {
        addDelta({
          proc,
          path: c.metadata.processPath,
          host: c.metadata.host || c.metadata.destinationIP,
          up: dUp,
          down: dDown,
          direct: c.chains?.[0] === 'DIRECT',
          isNewConn: !prev,
        });
      }
      const s = tickSpeed.get(proc) ?? { up: 0, down: 0 };
      s.up += dUp;
      s.down += dDown;
      tickSpeed.set(proc, s);
    }
    for (const id of live.keys()) if (!seen.has(id)) live.delete(id);

    // The core's global counters also include connections that opened and closed
    // between two polls, plus the final bytes of ones that just closed. Book the
    // gap so the daily total matches what Clash itself reports.
    if (lastGlobal && resp.uploadTotal >= lastGlobal.up && resp.downloadTotal >= lastGlobal.down) {
      const gapUp = resp.uploadTotal - lastGlobal.up - attributedUp;
      const gapDown = resp.downloadTotal - lastGlobal.down - attributedDown;
      if (gapUp > 0 || gapDown > 0) {
        addDelta({
          proc: UNATTRIBUTED,
          path: '',
          host: '',
          up: Math.max(0, gapUp),
          down: Math.max(0, gapDown),
          direct: false,
          isNewConn: false,
        });
      }
    }
    // First reading after (re)connect only sets the baseline; a counter drop means the core restarted.
    lastGlobal = { up: resp.uploadTotal, down: resp.downloadTotal };

    if (secs) {
      const globalUp = [...tickSpeed.values()].reduce((a, s) => a + s.up, 0);
      const globalDown = [...tickSpeed.values()].reduce((a, s) => a + s.down, 0);
      speed = { up: globalUp / secs, down: globalDown / secs };
      procSpeed = new Map([...tickSpeed].map(([k, s]) => [k, { up: s.up / secs, down: s.down / secs }]));
    }

    status = { connected: true, endpoint: client.endpointLabel, version: client.version, error: null };
  } catch (e) {
    status = {
      connected: false,
      endpoint: '',
      version: null,
      error: `无法连接 Clash Verge：${(e as Error).message}`,
    };
    live.clear();
    lastGlobal = null;
    lastTickAt = 0;
    speed = { up: 0, down: 0 };
    procSpeed.clear();
  } finally {
    busy = false;
  }
  listeners.forEach((fn) => fn());
}

export function getSnapshot(range: Range): Snapshot {
  const agg = aggregate(range);
  const liveCount = new Map<string, number>();
  for (const c of live.values()) liveCount.set(c.proc, (liveCount.get(c.proc) ?? 0) + 1);

  const procs: ProcRow[] = Object.entries(agg)
    .map(([name, s]) => ({
      name,
      path: s.path,
      up: s.up,
      down: s.down,
      direct: s.direct,
      conns: s.conns,
      liveConns: liveCount.get(name) ?? 0,
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
    status,
    totalUp: procs.reduce((a, p) => a + p.up, 0),
    totalDown: procs.reduce((a, p) => a + p.down, 0),
    totalDirect: procs.reduce((a, p) => a + p.direct, 0),
    speedUp: speed.up,
    speedDown: speed.down,
    procs,
    since: earliestDay(),
  };
}
