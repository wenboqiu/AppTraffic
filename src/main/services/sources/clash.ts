/**
 * Traffic source backed by a Clash-compatible /connections API (mihomo in Clash
 * Verge, Mihomo Party, FlClash…, or sing-box's clash_api). Connections are short-lived and vanish from the list once
 * closed, so we remember each id's last reading and only report the difference.
 */

import { MihomoClient, NotDetectedError, type MihomoConnection } from '../mihomo';
import { procName } from '../cores';
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
  private connected = false;
  private lastError: string | null = null;

  async poll(): Promise<ClashSample> {
    let resp;
    try {
      resp = await this.client.getConnections();
    } catch (e) {
      this.connected = false;
      this.lastError = e instanceof NotDetectedError ? null : (e as Error).message;
      throw e;
    }
    this.connected = true;
    this.lastError = null;
    const route = this.client.client ?? 'Clash';

    const deltas: ClashDelta[] = [];
    const seen = new Set<string>();
    let attributedUp = 0;
    let attributedDown = 0;
    // First reading after start or reconnect: connections already open carry bytes
    // from before (possibly counted by a previous run), so only record a baseline.
    const baseline = this.lastGlobal === null;

    for (const c of resp.connections ?? []) {
      seen.add(c.id);
      // mihomo labels its own provider/GeoIP downloads process "mihomo" with no path
      const own = c.metadata.process === 'mihomo' && !c.metadata.processPath;
      const via = own ? 'inner' : viaOf(c);
      // sing-box reports only processPath
      const exe = own ? '' : c.metadata.process || (c.metadata.processPath ? procName(c.metadata.processPath) : '');
      // "Inner" connections are ones the core opens itself (DNS upstreams, rule-provider updates…)
      const proc = exe || (via === 'inner' ? CORE_INNER : UNKNOWN_PROCESS);
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
        deltas.push({
          proc,
          path: c.metadata.processPath ?? '',
          up: dUp,
          down: dDown,
          direct: isDirect(c) ? dUp + dDown : 0,
          route,
          newConns: prev ? 0 : 1,
          hosts: [[c.metadata.host || c.metadata.destinationIP, dUp + dDown]],
          via,
          known: !!exe,
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
          route,
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
    return { route, tunDevice: await this.client.tunDevice(), deltas, liveConns };
  }

  /** Looking for a controller and none found yet: other sources should wait for it. */
  get searching(): boolean {
    return this.client.searching;
  }

  reset(): void {
    this.live.clear();
    this.lastGlobal = null;
  }

  status(): SourceStatus {
    const client = this.client.client ?? 'Clash';
    if (this.connected) {
      const version = this.client.version ? ` · ${this.client.version}` : '';
      return { id: this.id, label: '代理', state: 'on', detail: `${client}${version}`, action: null };
    }
    if (this.lastError) {
      return { id: this.id, label: '代理', state: 'error', detail: `${client} 连接失败：${this.lastError}`, action: null };
    }
    const detail = this.client.searching ? '正在检测代理…' : '未检测到 Clash 兼容的代理';
    return { id: this.id, label: '代理', state: 'off', detail, action: null };
  }
}

/** mihomo: "HTTP", "Socks5", "Tun", "Inner"…; sing-box: "mixed/mixed-in", "tun/tun-in"… */
function viaOf(c: MihomoConnection): ClashVia {
  const type = (c.metadata.type ?? '').toLowerCase();
  if (type === 'inner') return 'inner';
  if (type === 'tun' || type.startsWith('tun/')) return 'tun';
  return 'proxy';
}

/** The chain lists the outbound (and the groups that picked it); DIRECT means no proxy node. */
function isDirect(c: MihomoConnection): boolean {
  return (c.chains ?? []).some((name) => name.toLowerCase() === 'direct');
}
