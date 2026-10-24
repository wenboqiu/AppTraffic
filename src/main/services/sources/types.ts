/**
 * A traffic source turns some upstream view of the network (a proxy's connection
 * list, a system-level counter…) into per-process byte deltas. The collector
 * polls every source once per tick and merges what they report.
 */

import type { Delta } from '../store';
import type { SourceStatus } from '../../../shared/types';

export interface SourceSample {
  /** Bytes seen since the previous successful poll, already attributed to a process. */
  deltas: Delta[];
  /** Open connections per process right now (for the "N 个连接" hint). */
  liveConns: Map<string, number>;
}

export interface TrafficSource {
  readonly id: string;
  /** Throws when the source is unavailable; the collector records the error. */
  poll(): Promise<SourceSample>;
  /** Forget per-connection baselines after a failed poll so stale readings aren't diffed against. */
  reset(): void;
  status(): SourceStatus;
}
