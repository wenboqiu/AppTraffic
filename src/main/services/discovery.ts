/**
 * Finds a running Clash-compatible REST API (mihomo, Clash Meta, sing-box's
 * clash_api) without knowing which GUI client started it:
 *
 * 1. Running core processes: their command line names the config file (-f/-d for
 *    mihomo, -c/-D for sing-box) and sometimes the controller itself
 *    (-ext-ctl, -ext-ctl-pipe, -secret); the parent process names the client.
 * 2. Clash Verge's known config, for when its core runs as a service whose
 *    command line a non-elevated process can't read.
 * 3. Named pipes that look like a core's controller.
 *
 * Every candidate is confirmed with GET /version before use.
 */

import { existsSync, readFileSync, readdirSync } from 'fs';
import path from 'path';
import { CORE_NAMES, clientLabel } from './cores';
import { request, type Endpoint } from './mihomo';
import { runPowerShell } from './elevate';

const VERGE_DIR = path.join(process.env.APPDATA ?? '', 'io.github.clash-verge-rev.clash-verge-rev');
const PIPE_HINT = /mihomo|clash|verge|sing-?box|sparkle|party/i;
/** Our own pipes (single-instance lock, updater) */
const PIPE_IGNORE = /traffic/i;

export interface CoreProc {
  pid: number;
  name: string;
  parent: string | null;
  cmd: string | null;
}

interface ControllerConfig {
  controller?: string;
  pipe?: string;
  secret?: string;
}

async function listCores(): Promise<CoreProc[]> {
  const names = CORE_NAMES.map((n) => `'${n.toLowerCase()}'`).join(',');
  const script = `
$all = Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,Name,CommandLine
$names = @{}
foreach ($p in $all) { $names[[int]$p.ProcessId] = $p.Name }
$cores = @(${names})
$found = @($all | Where-Object { $cores -contains $_.Name.ToLower() } | ForEach-Object {
  [pscustomobject]@{ pid = [int]$_.ProcessId; name = $_.Name; parent = $names[[int]$_.ParentProcessId]; cmd = $_.CommandLine }
})
ConvertTo-Json -InputObject $found -Compress`;
  try {
    const out = (await runPowerShell(script)).trim();
    return out ? (JSON.parse(out) as CoreProc[]) : [];
  } catch {
    return [];
  }
}

/** Split a Windows command line into arguments, honouring double quotes. */
function tokenize(cmd: string): string[] {
  return [...cmd.matchAll(/"([^"]*)"|(\S+)/g)].map((m) => m[1] ?? m[2]);
}

function flagValues(tokens: string[]): Map<string, string> {
  const out = new Map<string, string>();
  for (let i = 0; i < tokens.length - 1; i++) {
    if (/^--?[A-Za-z]/.test(tokens[i]) && !out.has(tokens[i])) out.set(tokens[i], tokens[i + 1].replace(/^\\\\\?\\/, ''));
  }
  return out;
}

function readControllerConfig(file: string): ControllerConfig {
  if (!existsSync(file)) return {};
  let text: string;
  try {
    text = readFileSync(file, 'utf8');
  } catch {
    return {};
  }
  if (file.toLowerCase().endsWith('.json')) {
    // sing-box: experimental.clash_api
    try {
      const api = JSON.parse(text)?.experimental?.clash_api;
      return api ? { controller: api.external_controller, secret: api.secret } : {};
    } catch {
      return {};
    }
  }
  const field = (key: string) => {
    const m = text.match(new RegExp(`^${key}:[ \\t]*['"]?([^'"\\r\\n#]*?)['"]?[ \\t]*(#.*)?$`, 'm'));
    return m?.[1] || undefined;
  };
  return { controller: field('external-controller'), pipe: field('external-controller-pipe'), secret: field('secret') };
}

function httpEndpoint(controller: string | undefined, secret: string, client: string): Endpoint | null {
  const m = controller?.match(/^\[?([^\]]*?)\]?:(\d+)$/);
  if (!m) return null;
  // A wildcard bind is reachable on loopback
  const host = ['', '0.0.0.0', '::', '127.0.0.1', 'localhost'].includes(m[1]) ? '127.0.0.1' : m[1];
  return { kind: 'http', host, port: Number(m[2]), secret, client };
}

/** Exported for tests. */
export function endpointsFromCore(core: CoreProc): Endpoint[] {
  if (!core.cmd) return [];
  const flags = flagValues(tokenize(core.cmd));
  const dir = flags.get('-d') ?? flags.get('-D') ?? flags.get('--directory');
  let file = flags.get('-f') ?? flags.get('-c') ?? flags.get('--config');
  if (!file && dir) file = path.join(dir, 'config.yaml');
  else if (file && dir && !path.isAbsolute(file)) file = path.join(dir, file);
  const cfg = file ? readControllerConfig(file) : {};

  const client = clientLabel(core.parent, core.name);
  const secret = flags.get('-secret') ?? cfg.secret ?? '';
  const out: Endpoint[] = [];
  const pipe = flags.get('-ext-ctl-pipe') ?? cfg.pipe;
  if (pipe) out.push({ kind: 'pipe', socketPath: pipe, secret, client });
  const ep = httpEndpoint(flags.get('-ext-ctl') ?? cfg.controller, secret, client);
  if (ep) out.push(ep);
  return out;
}

function vergeEndpoints(): Endpoint[] {
  const file = path.join(VERGE_DIR, 'clash-verge.yaml');
  if (!existsSync(file)) return [];
  const cfg = readControllerConfig(file);
  const secret = cfg.secret ?? '';
  const out: Endpoint[] = [
    { kind: 'pipe', socketPath: cfg.pipe || '\\\\.\\pipe\\verge-mihomo', secret, client: 'Clash Verge' },
  ];
  const ep = httpEndpoint(cfg.controller, secret, 'Clash Verge');
  if (ep) out.push(ep);
  return out;
}

function pipeEndpoints(): Endpoint[] {
  let names: string[];
  try {
    names = readdirSync('\\\\.\\pipe\\');
  } catch {
    return [];
  }
  return names
    .filter((n) => PIPE_HINT.test(n) && !PIPE_IGNORE.test(n))
    .map((n) => {
      const client = /verge/i.test(n) ? 'Clash Verge' : /party/i.test(n) ? 'Mihomo Party' : /sparkle/i.test(n) ? 'Sparkle' : n;
      return { kind: 'pipe', socketPath: `\\\\.\\pipe\\${n}`, secret: '', client } satisfies Endpoint;
    });
}

function key(ep: Endpoint): string {
  return ep.kind === 'pipe' ? ep.socketPath.toLowerCase() : `${ep.host}:${ep.port}`;
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
  for (const core of await listCores()) add(endpointsFromCore(core));
  add(vergeEndpoints());
  add(pipeEndpoints());

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
