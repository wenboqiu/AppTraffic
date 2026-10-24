/**
 * macOS — not implemented yet; this stub only keeps the app running there with
 * whatever needs no OS support. To do (see README):
 * - listProcesses: `ps -axo pid=,ppid=,comm=` + argv via sysctl KERN_PROCARGS2
 *   (`ps -o args` loses the quoting of paths with spaces)
 * - knownEndpoints: Clash Verge's config under ~/Library/Application Support/
 *   io.github.clash-verge-rev.clash-verge-rev/; mihomo serves a Unix socket there
 * - socketEndpoints: Unix sockets of known clients
 * - systemProxyPorts: `scutil --proxy` (HTTPEnable/HTTPPort, SOCKSEnable/SOCKSPort)
 * - createSystemSource: `nettop` (per-process bytes, no root needed; per-flow mode
 *   also gives the interface, utunN for VPNs, and lo0 flows for local proxies)
 */

import type { Platform } from '../types';
import type { SystemSample, SystemSource } from '../../services/sources/types';
import type { SourceStatus } from '../../../shared/types';

class UnsupportedSystemSource implements SystemSource {
  readonly id = 'system';

  async poll(): Promise<SystemSample> {
    throw new Error('not supported on this platform yet');
  }

  reset(): void {}

  async autoStart(): Promise<void> {}

  async toggle(): Promise<void> {}

  status(): SourceStatus {
    return { id: this.id, label: '系统流量', state: 'off', detail: 'macOS 版尚未实现', action: null };
  }
}

export const darwin: Platform = {
  listProcesses: async () => [],
  knownEndpoints: () => [],
  socketEndpoints: () => [],
  systemProxyPorts: () => new Set(),
  createSystemSource: () => new UnsupportedSystemSource(),
};
