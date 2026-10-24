/**
 * Proxy cores that expose (or may expose) a Clash-compatible REST API, the GUI
 * clients that launch them, and VPN clients. Names are compared without the
 * Windows ".exe" suffix and case-insensitively, so one list serves every platform.
 */

import { UNKNOWN_PROCESS } from '../../shared/types';

/** Executable names of cores whose own sockets are the upstream side of app traffic. */
export const CORE_NAMES = [
  'verge-mihomo',
  'verge-mihomo-alpha',
  'mihomo',
  'mihomo-alpha',
  'mihomo-windows-amd64',
  'mihomo-darwin-arm64',
  'mihomo-darwin-amd64',
  'clash',
  'clash-meta',
  'clash-meta-alpha',
  'FlClashCore',
  'sing-box',
];

/**
 * VPN clients whose own traffic is the encrypted carrier of what apps send through
 * the VPN adapter, i.e. the same bytes a second time.
 */
const VPN_CLIENTS = [
  'openvpn',
  'openvpnserv',
  'wireguard',
  'wireguard-go',
  'tailscaled',
  'io.tailscale.ipn.macsys.network-extension',
  'zerotier-one',
  'zerotier-one_x64',
  'zerotier-one_x86',
  'vpnagent', // Cisco AnyConnect / Secure Client
  'vpnagentd',
  'PanGPS', // GlobalProtect
  'FortiSSLVPNdaemon',
  'openconnect',
  'hamachi-2',
  'nordvpn-service',
  'expressvpnd',
  'ProtonVPN.WireGuardService',
  'netbird',
  'easytier-core',
];

/** Friendly names for the parent process that launched a core. */
const CLIENT_LABELS: Record<string, string> = {
  'clash-verge': 'Clash Verge',
  'clash-verge-service': 'Clash Verge',
  'mihomo-party': 'Mihomo Party',
  sparkle: 'Sparkle',
  flclash: 'FlClash',
  flclashhelperservice: 'FlClash',
  v2rayn: 'v2rayN',
  v2rayu: 'V2rayU',
  nekoray: 'NekoRay',
  nekobox: 'NekoBox',
  hiddify: 'Hiddify',
  'clash-for-windows': 'Clash for Windows',
  clashx: 'ClashX',
  'clashx-meta': 'ClashX Meta',
  'gui.for.clash': 'GUI.for.Clash',
  'gui.for.singbox': 'GUI.for.SingBox',
};

/** "verge-mihomo.exe" -> "verge-mihomo" */
export function stem(name: string): string {
  return name.replace(/\.exe$/i, '');
}

const key = (name: string) => stem(name).toLowerCase();
const CORE_SET = new Set(CORE_NAMES.map(key));
const VPN_CLIENT_SET = new Set(VPN_CLIENTS.map(key));

/** File name of an executable path ("C:\\x\\a.exe" or "/usr/bin/a"). */
export function procName(exePath: string): string {
  if (!exePath) return UNKNOWN_PROCESS;
  return exePath.split(/[\\/]/).pop() || exePath;
}

export function isCore(name: string): boolean {
  return CORE_SET.has(key(name));
}

export function isVpnClient(name: string): boolean {
  return VPN_CLIENT_SET.has(key(name));
}

/** "Clash Verge" for a core started by clash-verge(.exe); the core's own stem otherwise. */
export function clientLabel(parentName: string | null, coreName: string): string {
  if (parentName) {
    const label = CLIENT_LABELS[key(parentName).replace(/[\s_]+/g, '-')];
    if (label) return label;
  }
  return stem(coreName);
}
