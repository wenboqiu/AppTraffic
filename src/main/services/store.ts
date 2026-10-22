/**
 * Per-day, per-process traffic buckets persisted to userData/traffic.json.
 */

import { app } from 'electron';
import path from 'path';
import { existsSync, readFileSync, writeFileSync, renameSync, mkdirSync } from 'fs';
import type { DayData, ProcStat, Range, StoreFile } from '../../shared/types';

const KEEP_DAYS = 90;
const HOSTS_KEEP = 100;
const HOSTS_TRIM_AT = 200;

let data: StoreFile = { version: 1, days: {} };
let dirty = false;

export function storePath(): string {
  return path.join(app.getPath('userData'), 'traffic.json');
}

export function dayKey(d = new Date()): string {
  const p = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

export function loadStore(): void {
  const file = storePath();
  if (!existsSync(file)) return;
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8')) as StoreFile;
    if (parsed?.version === 1 && parsed.days) data = parsed;
  } catch (e) {
    console.error('traffic.json unreadable, starting fresh:', e);
  }
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
  return { path: procPath, up: 0, down: 0, direct: 0, conns: 0, hosts: {} };
}

export interface Delta {
  proc: string;
  path: string;
  host: string;
  up: number;
  down: number;
  direct: boolean;
  isNewConn: boolean;
}

export function addDelta(d: Delta): void {
  const key = dayKey();
  const day = (data.days[key] ??= {});
  const stat = (day[d.proc] ??= emptyStat(d.path));
  if (!stat.path && d.path) stat.path = d.path;
  stat.up += d.up;
  stat.down += d.down;
  if (d.direct) stat.direct += d.up + d.down;
  if (d.isNewConn) stat.conns++;
  if (d.host && d.up + d.down > 0) {
    stat.hosts[d.host] = (stat.hosts[d.host] ?? 0) + d.up + d.down;
    if (Object.keys(stat.hosts).length > HOSTS_TRIM_AT) stat.hosts = topN(stat.hosts, HOSTS_KEEP);
  }
  dirty = true;
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
      acc.direct += s.direct;
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
