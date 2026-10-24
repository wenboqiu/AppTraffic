/**
 * Reading a core's controller settings from its config file: mihomo / Clash Meta
 * YAML, or sing-box JSON (experimental.clash_api).
 */

import { existsSync, readFileSync } from 'fs';
import type { Endpoint } from './mihomo';

export interface ControllerConfig {
  /** "127.0.0.1:9090", ":9090"… */
  controller?: string;
  /** Windows named pipe (external-controller-pipe) or Unix socket (external-controller-unix) */
  socket?: string;
  secret?: string;
}

export function readControllerConfig(file: string): ControllerConfig {
  if (!existsSync(file)) return {};
  let text: string;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    return {};
  }
  if (file.toLowerCase().endsWith('.json')) {
    try {
      const api = JSON.parse(text)?.experimental?.clash_api;
      return api ? { controller: api.external_controller, secret: api.secret } : {};
    } catch {
      return {};
    }
  }
  // Top-level keys only (indented "secret:" lines belong to proxies)
  const field = (key: string) => {
    const m = text.match(new RegExp(`^${key}:[ \\t]*['"]?([^'"\\r\\n#]*?)['"]?[ \\t]*(#.*)?$`, 'm'));
    return m?.[1] || undefined;
  };
  return {
    controller: field('external-controller'),
    socket: field('external-controller-pipe') ?? field('external-controller-unix'),
    secret: field('secret'),
  };
}

/** HTTP endpoint for a controller address; a wildcard bind is reached on loopback. */
export function httpEndpoint(controller: string | undefined, secret: string, client: string): Endpoint | null {
  const m = controller?.match(/^\[?([^\]]*?)\]?:(\d+)$/);
  if (!m) return null;
  const host = ['', '0.0.0.0', '::', '127.0.0.1', 'localhost'].includes(m[1]) ? '127.0.0.1' : m[1];
  return { kind: 'http', host, port: Number(m[2]), secret, client };
}

/** Endpoints a config file describes, socket first (no secret needed, no port clash). */
export function endpointsFromConfig(file: string, client: string): Endpoint[] {
  if (!existsSync(file)) return [];
  const cfg = readControllerConfig(file);
  const secret = cfg.secret ?? '';
  const out: Endpoint[] = [];
  if (cfg.socket) out.push({ kind: 'socket', socketPath: cfg.socket, secret, client });
  const ep = httpEndpoint(cfg.controller, secret, client);
  if (ep) out.push(ep);
  return out;
}
