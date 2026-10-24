export type Range = 'today' | '7d' | '30d' | 'all';

/** One process's accumulated traffic inside a single day bucket (persisted). */
export interface ProcStat {
  path: string;
  up: number;
  down: number;
  /** Bytes (up + down) that went out via DIRECT rather than a proxy node. */
  direct: number;
  conns: number;
  /** host -> bytes (up + down), trimmed to the top entries. */
  hosts: Record<string, number>;
}

export type DayData = Record<string, ProcStat>;

export interface StoreFile {
  version: 1;
  days: Record<string, DayData>;
}

export interface ProcRow {
  name: string;
  path: string;
  up: number;
  down: number;
  direct: number;
  conns: number;
  liveConns: number;
  speedUp: number;
  speedDown: number;
  topHosts: [string, number][];
}

export interface SourceStatus {
  id: string;
  label: string;
  connected: boolean;
  endpoint: string;
  version: string | null;
  error: string | null;
}

export interface Snapshot {
  range: Range;
  sources: SourceStatus[];
  totalUp: number;
  totalDown: number;
  totalDirect: number;
  speedUp: number;
  speedDown: number;
  procs: ProcRow[];
  /** Earliest day present in the store (YYYY-MM-DD), or null if empty. */
  since: string | null;
}

/** Bucket name for traffic seen in the core's global counters but not on any polled connection. */
export const UNATTRIBUTED = '(短连接/未归属)';
export const UNKNOWN_PROCESS = '(未识别进程)';
export const CORE_INNER = '(Clash 内核 · DNS 等)';
