export type Range = 'today' | '7d' | '30d' | 'all';

/** Route key for bytes that didn't go through any proxy or VPN. */
export const DIRECT = 'direct';
/**
 * Route key for a VPN client's (or TUN proxy core's) own carrier traffic: the
 * encrypted copy of bytes already counted under the apps, so left out of totals.
 */
export const TUNNEL = 'tunnel';
/** Prefix of route keys for traffic through a VPN adapter: "VPN · wg-home". */
export const VPN_ROUTE_PREFIX = 'VPN · ';

/** One process's accumulated traffic inside a single day bucket (persisted). */
export interface ProcStat {
  path: string;
  up: number;
  down: number;
  conns: number;
  /** host -> bytes (up + down), trimmed to the top entries. */
  hosts: Record<string, number>;
  /** Route -> bytes (up + down): DIRECT, or the proxy client that carried them ("Clash Verge"…). */
  routes: Record<string, number>;
}

export type DayData = Record<string, ProcStat>;

export interface StoreFile {
  version: 2;
  days: Record<string, DayData>;
}

export interface ProcRow {
  name: string;
  path: string;
  up: number;
  down: number;
  direct: number;
  /** VPN carrier bytes (route TUNNEL), not part of the totals. */
  tunnel: number;
  /** Proxy / VPN routes, largest first. */
  routes: [string, number][];
  conns: number;
  liveConns: number;
  speedUp: number;
  speedDown: number;
  topHosts: [string, number][];
}

export interface SourceStatus {
  id: string;
  /** What the source is: "系统流量", "代理" */
  label: string;
  /** on: delivering data; off: not running / not detected (normal); error: needs attention */
  state: 'on' | 'off' | 'error';
  /** "已启用", "Clash Verge · mihomo v1.19.2", "未检测到"… */
  detail: string;
  /** Label of a button that turns the source on, if it can be. */
  action: string | null;
}

export interface Snapshot {
  range: Range;
  sources: SourceStatus[];
  totalUp: number;
  totalDown: number;
  totalDirect: number;
  /** VPN carrier traffic, already left out of totalUp / totalDown. */
  totalTunnel: number;
  /** Proxy / VPN routes across all processes, largest first. */
  routes: [string, number][];
  speedUp: number;
  speedDown: number;
  procs: ProcRow[];
  /** Earliest day present in the store (YYYY-MM-DD), or null if empty. */
  since: string | null;
}

/** Bucket name for traffic seen in the core's global counters but not on any polled connection. */
export const UNATTRIBUTED = '(短连接/未归属)';
export const UNKNOWN_PROCESS = '(未识别进程)';
export const CORE_INNER = '(代理内核 · DNS 等)';
/** v1 data came only from Clash Verge; its proxied bytes are migrated to this route. */
export const LEGACY_ROUTE = 'Clash Verge';
