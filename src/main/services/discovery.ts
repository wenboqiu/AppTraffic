/**
 * Finds a running Clash-compatible REST API (mihomo, Clash Meta, sing-box's
 * clash_api) without knowing which GUI client started it:
 *
 * 1. Running core processes: their command line names the config file (-f/-d for
 *    mihomo, -c/-D for sing-box) and sometimes the controller itself
 *    (-ext-ctl, -ext-ctl-pipe / -ext-ctl-unix, -secret); the parent process names
 *    the client.
 * 2. Well-known client config locations, for cores whose command line can't be
 *    read (running as a service / another user).
 * 3. Local sockets that look like a core's controller.
 *
 * Every candidate is confirmed with GET /version before use. The OS-specific
 * parts (listing processes, where configs and sockets live) are in platform/.
 */

import path from 'path';
import { CORE_NAMES, clientLabel } from './cores';
import { request, type Endpoint } from './mihomo';
import { httpEndpoint, readControllerConfig } from './controller-config';
import { platform } from '../platform';
import type { CoreProc } from '../platform/types';

function flagValues(args: string[]): Map<string, string> {
  const out = new Map<string, string>();
  for (let i = 0; i < args.length - 1; i++) {
    // \\?\ is a Windows long-path prefix some launchers add
    if (/^--?[A-Za-z]/.test(args[i]) && !out.has(args[i])) out.set(args[i], args[i + 1].replace(/^\\\\\?\\/, ''));
  }
  return out;
}

/** Exported for tests. */
export function endpointsFromCore(core: CoreProc): Endpoint[] {
  if (!core.args) return [];
  const flags = flagValues(core.args);
  const dir = flags.get('-d') ?? flags.get('-D') ?? flags.get('--directory');
  let file = flags.get('-f') ?? flags.get('-c') ?? flags.get('--config');
  if (!file && dir) file = path.join(dir, 'config.yaml');
  else if (file && dir && !path.isAbsolute(file)) file = path.join(dir, file);
  const cfg = file ? readControllerConfig(file) : {};

  const client = clientLabel(core.parent, core.name);
  const secret = flags.get('-secret') ?? cfg.secret ?? '';
  const out: Endpoint[] = [];
  const socket = flags.get('-ext-ctl-pipe') ?? flags.get('-ext-ctl-unix') ?? cfg.socket;
  if (socket) out.push({ kind: 'socket', socketPath: socket, secret, client });
  const ep = httpEndpoint(flags.get('-ext-ctl') ?? cfg.controller, secret, client);
  if (ep) out.push(ep);
  return out;
}

function key(ep: Endpoint): string {
  return ep.kind === 'socket' ? ep.socketPath.toLowerCase() : `${ep.host}:${ep.port}`;
}

export interface Discovered {
  endpoint: Endpoint;
  version: string;
}

/** Probe all candidates in order of confidence; resolves to the first that answers. */
export async function discover(): Promise<Discovered | null> {
  const candidates: Endpoint[] = [];
  const seen = new Set<string>();
  const add = (eps: Endpoint[]) => {
    for (const ep of eps) {
      if (seen.has(key(ep))) continue;
      seen.add(key(ep));
      candidates.push(ep);
    }
  };
  for (const core of await platform.listProcesses(CORE_NAMES)) add(endpointsFromCore(core));
  add(platform.knownEndpoints());
  add(platform.socketEndpoints());

  for (const ep of candidates) {
    try {
      const v = await request<{ version?: string }>(ep, '/version', 1500);
      if (typeof v?.version === 'string') return { endpoint: ep, version: v.version };
    } catch {
      // not a controller, wrong secret, or not running
    }
  }
  return null;
}
