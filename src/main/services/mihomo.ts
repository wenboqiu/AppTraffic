/**
 * Minimal client for a Clash-compatible REST API (mihomo / Clash Meta, sing-box
 * clash_api), over a named pipe or HTTP. discovery.ts finds the endpoint.
 */

import http from 'http';
import { discover } from './discovery';

/** Re-run discovery at most this often while no controller is reachable. */
const REDISCOVER_MS = 10_000;
/** TUN settings change rarely; re-read them this often. */
const CONFIG_TTL_MS = 30_000;

export interface MihomoConnection {
  id: string;
  upload: number;
  download: number;
  chains: string[];
  metadata: {
    /** mihomo: "HTTP", "Tun", "Inner"…; sing-box: "<inbound type>/<tag>" */
    type: string;
    host: string;
    destinationIP: string;
    /** mihomo only; sing-box reports processPath alone */
    process?: string;
    processPath?: string;
  };
}

export interface ConnectionsResponse {
  uploadTotal: number;
  downloadTotal: number;
  connections: MihomoConnection[] | null;
}

export type Endpoint = ({ kind: 'pipe'; socketPath: string } | { kind: 'http'; host: string; port: number }) & {
  secret: string;
  /** GUI client that runs the core, e.g. "Clash Verge" */
  client: string;
};

export function describeEndpoint(ep: Endpoint): string {
  return ep.kind === 'pipe' ? ep.socketPath : `http://${ep.host}:${ep.port}`;
}

export function request<T>(ep: Endpoint, apiPath: string, timeoutMs = 3000): Promise<T> {
  return new Promise((resolve, reject) => {
    const headers = ep.secret ? { Authorization: `Bearer ${ep.secret}` } : {};
    const opts: http.RequestOptions =
      ep.kind === 'pipe'
        ? { socketPath: ep.socketPath, path: apiPath, headers }
        : { host: ep.host, port: ep.port, path: apiPath, headers };
    const req = http.get({ ...opts, timeout: timeoutMs }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (c) => (body += c));
      res.on('end', () => {
        if (res.statusCode !== 200) return reject(new Error(`HTTP ${res.statusCode}`));
        try {
          resolve(JSON.parse(body) as T);
        } catch (e) {
          reject(e);
        }
      });
    });
    req.on('timeout', () => req.destroy(new Error('timeout')));
    req.on('error', reject);
  });
}

export class NotDetectedError extends Error {
  constructor() {
    super('未检测到 Clash 兼容的代理');
  }
}

export class MihomoClient {
  private endpoint: Endpoint | null = null;
  private lastDiscovery = 0;
  private discovering: Promise<void> | null = null;
  version: string | null = null;
  private tun: { device: string | null; at: number } = { device: null, at: 0 };

  get endpointLabel(): string {
    return this.endpoint ? describeEndpoint(this.endpoint) : '';
  }

  get client(): string | null {
    return this.endpoint?.client ?? null;
  }

  /** Discovery is running and nothing has been found yet. */
  get searching(): boolean {
    return this.discovering !== null && !this.endpoint;
  }

  private async connect(): Promise<Endpoint> {
    if (!this.discovering && Date.now() - this.lastDiscovery >= REDISCOVER_MS) {
      this.lastDiscovery = Date.now();
      this.discovering = discover()
        .then((found) => {
          this.endpoint = found?.endpoint ?? null;
          this.version = found?.version ?? null;
        })
        .finally(() => (this.discovering = null));
    }
    // Discovery spawns PowerShell and can take a second; don't hold up the tick for it
    if (!this.endpoint) throw new NotDetectedError();
    return this.endpoint;
  }

  /** The core's TUN adapter name when TUN mode is on; null otherwise or if unknown (sing-box). */
  async tunDevice(): Promise<string | null> {
    const ep = this.endpoint;
    if (!ep) return null;
    if (Date.now() - this.tun.at < CONFIG_TTL_MS) return this.tun.device;
    this.tun.at = Date.now();
    try {
      const cfg = await request<{ tun?: { enable?: boolean; device?: string } }>(ep, '/configs');
      this.tun.device = cfg.tun?.enable ? cfg.tun.device || null : null;
    } catch {
      // keep the previous answer
    }
    return this.tun.device;
  }

  async getConnections(): Promise<ConnectionsResponse> {
    const ep = this.endpoint ?? (await this.connect());
    try {
      return await request<ConnectionsResponse>(ep, '/connections');
    } catch (e) {
      // Drop the endpoint so the next poll re-discovers (core restarted, port changed, client switched…)
      this.endpoint = null;
      this.version = null;
      this.lastDiscovery = 0;
      throw e;
    }
  }
}
