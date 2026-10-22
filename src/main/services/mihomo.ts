/**
 * Minimal client for the mihomo (Clash Meta) REST API that Clash Verge Rev runs.
 * Prefers the named pipe Clash Verge always exposes; falls back to the HTTP
 * external-controller if one is configured.
 */

import http from 'http';
import path from 'path';
import { existsSync, readFileSync } from 'fs';

const VERGE_DIR = path.join(process.env.APPDATA ?? '', 'io.github.clash-verge-rev.clash-verge-rev');
const DEFAULT_PIPE = '\\\\.\\pipe\\verge-mihomo';

export interface MihomoConnection {
  id: string;
  upload: number;
  download: number;
  chains: string[];
  metadata: {
    type: string;
    host: string;
    destinationIP: string;
    process: string;
    processPath: string;
  };
}

export interface ConnectionsResponse {
  uploadTotal: number;
  downloadTotal: number;
  connections: MihomoConnection[] | null;
}

type Endpoint =
  | { kind: 'pipe'; socketPath: string }
  | { kind: 'http'; host: string; port: number; secret: string };

interface VergeConfig {
  pipe: string;
  controller: string;
  secret: string;
}

function readVergeConfig(): VergeConfig {
  const file = path.join(VERGE_DIR, 'clash-verge.yaml');
  const cfg: VergeConfig = { pipe: DEFAULT_PIPE, controller: '', secret: '' };
  if (!existsSync(file)) return cfg;
  const text = readFileSync(file, 'utf8');
  const field = (key: string) => {
    const m = text.match(new RegExp(`^${key}:\\s*['"]?([^'"\\r\\n]*?)['"]?\\s*$`, 'm'));
    return m ? m[1] : '';
  };
  cfg.pipe = field('external-controller-pipe') || DEFAULT_PIPE;
  cfg.controller = field('external-controller');
  cfg.secret = field('secret');
  return cfg;
}

function candidateEndpoints(): Endpoint[] {
  const cfg = readVergeConfig();
  const list: Endpoint[] = [{ kind: 'pipe', socketPath: cfg.pipe }];
  const m = cfg.controller.match(/^([^:]*):(\d+)$/);
  if (m) {
    const host = !m[1] || m[1] === '0.0.0.0' ? '127.0.0.1' : m[1];
    list.push({ kind: 'http', host, port: Number(m[2]), secret: cfg.secret });
  }
  return list;
}

export function describeEndpoint(ep: Endpoint): string {
  return ep.kind === 'pipe' ? ep.socketPath : `http://${ep.host}:${ep.port}`;
}

function request<T>(ep: Endpoint, apiPath: string, timeoutMs = 3000): Promise<T> {
  return new Promise((resolve, reject) => {
    const opts: http.RequestOptions =
      ep.kind === 'pipe'
        ? { socketPath: ep.socketPath, path: apiPath }
        : {
            host: ep.host,
            port: ep.port,
            path: apiPath,
            headers: ep.secret ? { Authorization: `Bearer ${ep.secret}` } : {},
          };
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

export class MihomoClient {
  private endpoint: Endpoint | null = null;
  version: string | null = null;

  get endpointLabel(): string {
    return this.endpoint ? describeEndpoint(this.endpoint) : '';
  }

  /** Find a working endpoint (re-read on every call so config changes are picked up). */
  private async connect(): Promise<Endpoint> {
    let lastErr: unknown = new Error('no endpoint');
    for (const ep of candidateEndpoints()) {
      try {
        const v = await request<{ version: string }>(ep, '/version');
        this.endpoint = ep;
        this.version = v.version;
        return ep;
      } catch (e) {
        lastErr = e;
      }
    }
    throw lastErr;
  }

  async getConnections(): Promise<ConnectionsResponse> {
    const ep = this.endpoint ?? (await this.connect());
    try {
      return await request<ConnectionsResponse>(ep, '/connections');
    } catch (e) {
      // Drop the cached endpoint so the next poll re-discovers (Clash restarted, port changed…)
      this.endpoint = null;
      this.version = null;
      throw e;
    }
  }
}
