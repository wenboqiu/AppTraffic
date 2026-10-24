/**
 * Traffic source backed by mihomo's /connections (Clash Verge Rev and other
 * Clash-API cores). Connections are short-lived and vanish from the list once
 * closed, so we remember each id's last reading and only report the difference.
 */

import { MihomoClient } from '../mihomo';
import { CORE_INNER, UNATTRIBUTED, UNKNOWN_PROCESS } from '../../../shared/types';
import type { SourceStatus } from '../../../shared/types';
import type { ClashDelta, ClashSample, ClashVia, TrafficSource } from './types';

interface LiveConn {
  proc: string;
  up: number;
  down: number;
}

export class ClashSource implements TrafficSource<ClashSample> {
  readonly id = 'clash';
  private client = new MihomoClient();
  private live = new Map<string, LiveConn>();
  private lastGlobal: { up: number; down: number } | null = null;
  private lastError: string | null = '正在连接 Clash…';

  async poll(): Promise<ClashSample> {
    let resp;
    try {
      resp = await this.client.getConnections();
    } catch (e) {
      this.lastError = `无法连接 Clash Verge：${(e as Error).message}`;
      throw e;
    }
    this.lastError = null;

    const deltas: ClashDelta[] = [];
    const seen = new Set<string>();
    let attributedUp = 0;
    let attributedDown = 0;
    // First reading after start or reconnect: connections already open carry bytes
    // from before (possibly counted by a previous run), so only record a baseline.
    const baseline = this.lastGlobal === null;

    for (const c of resp.connections ?? []) {
      seen.add(c.id);
      // "Inner" connections are ones the core opens itself (DNS upstreams, rule-provider updates…)
      const proc = c.metadata.process || (c.metadata.type === 'Inner' ? CORE_INNER : UNKNOWN_PROCESS);
      if (baseline) {
        this.live.set(c.id, { proc, up: c.upload, down: c.download });
        continue;
      }
      const prev = this.live.get(c.id);
      const dUp = Math.max(0, c.upload - (prev?.up ?? 0));
      const dDown = Math.max(0, c.download - (prev?.down ?? 0));
      this.live.set(c.id, { proc, up: c.upload, down: c.download });

      attributedUp += dUp;
      attributedDown += dDown;
      if (!prev || dUp || dDown) {
        const via: ClashVia =
          c.metadata.type === 'Inner' ? 'inner' : c.metadata.type === 'Tun' ? 'tun' : 'proxy';
        deltas.push({
          proc,
          path: c.metadata.processPath,
          up: dUp,
          down: dDown,
          direct: c.chains?.[0] === 'DIRECT' ? dUp + dDown : 0,
          newConns: prev ? 0 : 1,
          hosts: [[c.metadata.host || c.metadata.destinationIP, dUp + dDown]],
          via,
          known: !!c.metadata.process,
        });
      }
    }
    for (const id of this.live.keys()) if (!seen.has(id)) this.live.delete(id);

    // The core's global counters also include connections that opened and closed
    // between two polls, plus the final bytes of ones that just closed. Book the
    // gap so the daily total matches what Clash itself reports.
    const last = this.lastGlobal;
    if (last && resp.uploadTotal >= last.up && resp.downloadTotal >= last.down) {
      const gapUp = resp.uploadTotal - last.up - attributedUp;
      const gapDown = resp.downloadTotal - last.down - attributedDown;
      if (gapUp > 0 || gapDown > 0) {
        deltas.push({
          proc: UNATTRIBUTED,
          path: '',
          up: Math.max(0, gapUp),
          down: Math.max(0, gapDown),
          direct: 0,
          newConns: 0,
          hosts: [],
          via: 'gap',
          known: false,
        });
      }
    }
    // A counter drop means the core restarted; the next poll diffs against this reading.
    this.lastGlobal = { up: resp.uploadTotal, down: resp.downloadTotal };

    const liveConns = new Map<string, number>();
    for (const c of this.live.values()) liveConns.set(c.proc, (liveConns.get(c.proc) ?? 0) + 1);
    return { deltas, liveConns };
  }

  reset(): void {
    this.live.clear();
    this.lastGlobal = null;
  }

  status(): SourceStatus {
    return {
      id: this.id,
      label: 'Clash',
      connected: this.lastError === null,
      endpoint: this.client.endpointLabel,
      version: this.client.version,
      error: this.lastError,
      action: null,
    };
  }
}
