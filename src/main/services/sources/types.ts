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
  /** Name of the core's TUN adapter when TUN mode is on (mihomo /configs), else null. */
  tunDevice: string | null;
  /** Bytes seen since the previous successful poll. */
  deltas: ClashDelta[];
  /** Open connections per process right now (for the "N 个连接" hint). */
  liveConns: Map<string, number>;
}

// ── System-wide per-process counters (Windows: ETW; macOS: nettop) ──

export interface SysProc {
  /** Executable path; '' when the process couldn't be opened. */
  path: string;
  up: number;
  down: number;
  hosts: [string, number][];
  /** [interface index, up, down]; index 0 = local address not on any known interface */
  ifs?: [number, number, number][];
}

export interface SysIface {
  index: number;
  /** "WLAN", "wg-home"… */
  alias: string;
  desc: string;
  /** Virtual adapter that tunnels traffic elsewhere (VPN, TUN proxy, overlay network) */
  vpn: boolean;
}

/** Loopback TCP from `client` to the process listening on `port`. */
export interface SysLoop {
  client: string;
  server: string;
  port: number;
  /** client -> server */
  up: number;
  /** server -> client */
  down: number;
}

export interface SystemSample {
  procs: SysProc[];
  loops: SysLoop[];
  /** Interfaces referenced by procs[].ifs */
  ifaces: SysIface[];
}

/** The platform's system-wide source; it may need the user to turn it on. */
export interface SystemSource extends TrafficSource<SystemSample> {
  /** On app start: resume if the user enabled it before, without prompting. */
  autoStart(): Promise<void>;
  /** The source's status button (enable / disable). */
  toggle(): Promise<void>;
}
