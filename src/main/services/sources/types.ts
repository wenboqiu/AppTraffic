/**
 * A traffic source turns some upstream view of the network (a proxy's connection
 * list, a system-level counter…) into per-process byte samples. The collector
 * polls every source once per tick and merge.ts reconciles what they report.
 */

import type { Delta } from '../store';
import type { SourceStatus } from '../../../shared/types';

export interface TrafficSource<S> {
  readonly id: string;
  /** Throws when the source is unavailable; the collector records the error. */
  poll(): Promise<S>;
  /** Forget per-connection baselines after a failed poll so stale readings aren't diffed against. */
  reset(): void;
  status(): SourceStatus;
}

/**
 * How a Clash connection reached the core, which decides how it overlaps with ETW:
 * - proxy: app -> local HTTP/SOCKS port; the app's side is loopback, invisible to ETW
 * - tun:   captured by the TUN adapter; ETW also sees it under the real app
 * - inner: opened by the core itself (DNS upstreams, provider updates…)
 * - gap:   bytes in the core's global counters that no polled connection explains
 */
export type ClashVia = 'proxy' | 'tun' | 'inner' | 'gap';

export interface ClashDelta extends Delta {
  via: ClashVia;
  /** False when mihomo couldn't tell which process opened the connection. */
  known: boolean;
}

export interface ClashSample {
  /** Route label for proxied bytes: the client running the core ("Clash Verge"…). */
  route: string;
  /** Bytes seen since the previous successful poll. */
  deltas: ClashDelta[];
  /** Open connections per process right now (for the "N 个连接" hint). */
  liveConns: Map<string, number>;
}

export interface EtwProc {
  /** Executable path; '' when the process couldn't be opened. */
  path: string;
  up: number;
  down: number;
  hosts: [string, number][];
}

/** Loopback TCP from `client` to the process listening on `port`. */
export interface EtwLoop {
  client: string;
  server: string;
  port: number;
  /** client -> server */
  up: number;
  /** server -> client */
  down: number;
}

export interface EtwSample {
  procs: EtwProc[];
  loops: EtwLoop[];
}
