/**
 * Per-day, per-process traffic buckets persisted to userData/traffic.json.
 */

import { app } from 'electron';
import path from 'path';
import { copyFileSync, existsSync, readFileSync, writeFileSync, renameSync, mkdirSync } from 'fs';
import { CORE_INNER, DIRECT, LEGACY_ROUTE } from '../../shared/types';
import type { DayData, ProcStat, Range, StoreFile } from '../../shared/types';

const KEEP_DAYS = 90;
const HOSTS_KEEP = 100;
const HOSTS_TRIM_AT = 200;

let data: StoreFile = { version: 2, days: {} };
let dirty = false;

export function storePath(): string {
  return path.join(app.getPath('userData'), 'traffic.json');
}

export function dayKey(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/**
 * First run after the rename from ClashTraffic: copy its history over. The old
 * file is left in place, so ClashTraffic keeps working until it's uninstalled.
 */
export function importLegacyStore(): void {
  const target = storePath();
  const legacy = path.join(app.getPath('appData'), 'clash-traffic', 'traffic.json');
  if (existsSync(target) || !existsSync(legacy)) return;
  mkdirSync(path.dirname(target), { recursive: true });
  copyFileSync(legacy, target);
}

export function loadStore(): void {
  const file = storePath();
  if (!existsSync(file)) return;
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8'));
    if (parsed?.version === 2 && parsed.days) data = parsed as StoreFile;
    else if (parsed?.version === 1 && parsed.days) {
      // Keep the original so a downgrade can still read it
      copyFileSync(file, `${file}.v1.bak`);
      data = migrateV1(parsed);
      dirty = true;
    }
  } catch (e) {
    console.error('traffic.json unreadable, starting fresh:', e);
  }
}

interface ProcStatV1 {
  path: string;
  up: number;
  down: number;
  direct: number;
  conns: number;
  hosts: Record<string, number>;
}

/** v1 (ClashTraffic, the app's former name) kept a single direct counter; everything else went through Clash Verge. */
function migrateV1(v1: { days: Record<string, Record<string, ProcStatV1>> }): StoreFile {
  const days: Record<string, DayData> = {};
  for (const [k, day] of Object.entries(v1.days)) {
    const out: DayData = (days[k] = {});
    for (const [proc, s] of Object.entries(day)) {
      const proxied = Math.max(0, s.up + s.down - s.direct);
      const routes: Record<string, number> = {};
      if (s.direct > 0) routes[DIRECT] = s.direct;
      if (proxied > 0) routes[LEGACY_ROUTE] = proxied;
      // The core's own traffic: v1's inner bucket and mihomo's pathless "mihomo" downloads
      const own = proc === '(Clash 内核 · DNS 等)' || (proc === 'mihomo' && !s.path);
      const name = own ? CORE_INNER : proc;
      const acc = (out[name] ??= emptyStat(s.path));
      acc.up += s.up;
      acc.down += s.down;
      acc.conns += s.conns;
      for (const [h, b] of Object.entries(s.hosts)) acc.hosts[h] = (acc.hosts[h] ?? 0) + b;
      for (const [r, b] of Object.entries(routes)) acc.routes[r] = (acc.routes[r] ?? 0) + b;
    }
  }
  return { version: 2, days };
}

export function saveStore(force = false): void {
  if (!dirty && !force) return;
  prune();
  const file = storePath();
  mkdirSync(path.dirname(file), { recursive: true });
  // Write-then-rename so a crash mid-write never corrupts the history
  const tmp = `${file}.tmp`;
  writeFileSync(tmp, JSON.stringify(data));
  renameSync(tmp, file);
  dirty = false;
}

function prune(): void {
  const cutoff = new Date();
  cutoff.setDate(cutoff.getDate() - KEEP_DAYS);
  const cutKey = dayKey(cutoff);
  for (const k of Object.keys(data.days)) if (k < cutKey) delete data.days[k];
}

function emptyStat(procPath: string): ProcStat {
  return { path: procPath, up: 0, down: 0, conns: 0, hosts: {}, routes: {} };
}

export interface Delta {
  proc: string;
  path: string;
  up: number;
  down: number;
  /** Part of up + down that went out directly rather than through a proxy. */
  direct: number;
  /** Proxy client that carried the rest ("Clash Verge"…); '' when nothing was proxied. */
  route: string;
  newConns: number;
  /** host -> bytes; may cover only part of up + down. */
  hosts: [string, number][];
}

export function addDelta(d: Delta): void {
  const key = dayKey();
  const day = (data.days[key] ??= {});
  const stat = (day[d.proc] ??= emptyStat(d.path));
  if (!stat.path && d.path) stat.path = d.path;
  stat.up += d.up;
  stat.down += d.down;
  // A negative direct moves bytes ETW booked early over to proxied (see merge.ts)
  addRoute(stat, DIRECT, d.direct);
  addRoute(stat, d.route || 'proxy', d.up + d.down - d.direct);
  stat.conns += d.newConns;
  for (const [host, bytes] of d.hosts) {
    if (!host || bytes <= 0) continue;
    stat.hosts[host] = (stat.hosts[host] ?? 0) + bytes;
  }
  if (Object.keys(stat.hosts).length > HOSTS_TRIM_AT) stat.hosts = topN(stat.hosts, HOSTS_KEEP);
  dirty = true;
}

function addRoute(stat: ProcStat, route: string, bytes: number): void {
  if (!bytes) return;
  const next = Math.max(0, (stat.routes[route] ?? 0) + bytes);
  if (next > 0) stat.routes[route] = next;
  else delete stat.routes[route];
}

function topN(rec: Record<string, number>, n: number): Record<string, number> {
  return Object.fromEntries(Object.entries(rec).sort((a, b) => b[1] - a[1]).slice(0, n));
}

function rangeStartKey(range: Range): string | null {
  if (range === 'all') return null;
  const d = new Date();
  if (range === '7d') d.setDate(d.getDate() - 6);
  if (range === '30d') d.setDate(d.getDate() - 29);
  return dayKey(d);
}

/** Merge all day buckets within the range into a single process map. */
export function aggregate(range: Range): DayData {
  const start = rangeStartKey(range);
  const out: DayData = {};
  for (const [k, day] of Object.entries(data.days)) {
    if (start && k < start) continue;
    for (const [proc, s] of Object.entries(day)) {
      const acc = (out[proc] ??= emptyStat(s.path));
      if (!acc.path && s.path) acc.path = s.path;
      acc.up += s.up;
      acc.down += s.down;
      for (const [r, b] of Object.entries(s.routes)) acc.routes[r] = (acc.routes[r] ?? 0) + b;
      acc.conns += s.conns;
      for (const [h, b] of Object.entries(s.hosts)) acc.hosts[h] = (acc.hosts[h] ?? 0) + b;
    }
  }
  return out;
}

export function earliestDay(): string | null {
  const keys = Object.keys(data.days).sort();
  return keys[0] ?? null;
}
