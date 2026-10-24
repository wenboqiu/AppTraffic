/**
 * Windows: running processes with their parent and command line (WMI via PowerShell),
 * plus the places Clash clients keep their controller.
 */

import path from 'path';
import { existsSync, readdirSync } from 'fs';
import { runPowerShell } from './elevate';
import { endpointsFromConfig, readControllerConfig } from '../../services/controller-config';
import type { Endpoint } from '../../services/mihomo';
import type { CoreProc } from '../types';

const PIPE_HINT = /mihomo|clash|verge|sing-?box|sparkle|party/i;
/** Our own pipes (single-instance lock, updater) */
const PIPE_IGNORE = /traffic/i;

/** Split a Windows command line into arguments, honouring double quotes. */
export function tokenize(cmd: string): string[] {
  return [...cmd.matchAll(/"([^"]*)"|(\S+)/g)].map((m) => m[1] ?? m[2]);
}

export async function listProcesses(names: string[]): Promise<CoreProc[]> {
  const wanted = names.map((n) => `'${n.toLowerCase().replace(/'/g, "''")}'`).join(',');
  const script = `
$all = Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,Name,CommandLine
$names = @{}
foreach ($p in $all) { $names[[int]$p.ProcessId] = $p.Name }
$wanted = @(${wanted})
$found = @($all | Where-Object { $wanted -contains [IO.Path]::GetFileNameWithoutExtension($_.Name).ToLower() } | ForEach-Object {
  [pscustomobject]@{ pid = [int]$_.ProcessId; name = $_.Name; parent = $names[[int]$_.ParentProcessId]; cmd = $_.CommandLine }
})
ConvertTo-Json -InputObject $found -Compress`;
  try {
    const out = (await runPowerShell(script)).trim();
    const rows = out ? (JSON.parse(out) as { pid: number; name: string; parent: string | null; cmd: string | null }[]) : [];
    return rows.map((r) => ({ pid: r.pid, name: r.name, parent: r.parent, args: r.cmd ? tokenize(r.cmd) : null }));
  } catch {
    return [];
  }
}

/** Clash Verge Rev's config, for when its core runs as a service. */
export function knownEndpoints(): Endpoint[] {
  const file = path.join(process.env.APPDATA ?? '', 'io.github.clash-verge-rev.clash-verge-rev', 'clash-verge.yaml');
  if (!existsSync(file)) return [];
  const eps = endpointsFromConfig(file, 'Clash Verge');
  // Verge always serves this pipe, whether or not the config names it
  if (!eps.some((e) => e.kind === 'socket')) {
    const secret = readControllerConfig(file).secret ?? '';
    eps.unshift({ kind: 'socket', socketPath: '\\\\.\\pipe\\verge-mihomo', secret, client: 'Clash Verge' });
  }
  return eps;
}

export function socketEndpoints(): Endpoint[] {
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
      return { kind: 'socket', socketPath: `\\\\.\\pipe\\${n}`, secret: '', client } satisfies Endpoint;
    });
}
