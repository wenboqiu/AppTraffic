import { useState } from 'react';
import { api } from '../api';
import type { ProcRow } from '../../../shared/types';
import { fmtBytes, fmtSpeed } from '../../../shared/format';

export default function ProcessList({ procs }: { procs: ProcRow[] }) {
  const [open, setOpen] = useState<string | null>(null);
  const max = procs.reduce((m, p) => Math.max(m, p.up + p.down), 0);

  if (procs.length === 0) {
    return <div className="empty-hint">还没有流量记录</div>;
  }

  return (
    <div className="proc-list">
      {procs.map((p) => {
        const total = p.up + p.down;
        const proxied = total - p.direct - p.tunnel;
        const isTunnel = p.tunnel > total / 2;
        const active = p.speedUp + p.speedDown > 0;
        const expanded = open === p.name;
        return (
          <div key={p.name} className={`proc ${expanded ? 'expanded' : ''}`}>
            <div className="proc-row" onClick={() => setOpen(expanded ? null : p.name)}>
              <div className="proc-line">
                <span className="proc-name" title={p.path || p.name}>
                  {p.name.replace(/\.exe$/i, '')}
                  {isTunnel && <span className="proc-tag">VPN 隧道</span>}
                </span>
                <span className="proc-total">{fmtBytes(total)}</span>
              </div>
              <div className="proc-bar-track">
                <div
                  className="proc-bar-direct"
                  style={{ width: `${max ? (p.direct / max) * 100 : 0}%` }}
                />
                <div className="proc-bar-proxy" style={{ width: `${max ? (proxied / max) * 100 : 0}%` }} />
                <div className="proc-bar-tunnel" style={{ width: `${max ? (p.tunnel / max) * 100 : 0}%` }} />
              </div>
              <div className="proc-sub">
                <span>
                  ↓ {fmtBytes(p.down)} · ↑ {fmtBytes(p.up)}
                </span>
                {active ? (
                  <span className="proc-speed">
                    ↓ {fmtSpeed(p.speedDown)} ↑ {fmtSpeed(p.speedUp)}
                  </span>
                ) : (
                  p.liveConns > 0 && <span>{p.liveConns} 个连接</span>
                )}
              </div>
            </div>

            {expanded && (
              <div className="proc-detail">
                {p.path && (
                  <div className="proc-path" onClick={() => api.showInFolder(p.path)}>
                    {p.path}
                  </div>
                )}
                <div className="proc-meta">
                  {[
                    p.conns > 0 && `累计 ${p.conns} 个连接`,
                    `直连 ${fmtBytes(p.direct)}`,
                    ...p.routes.map(([r, b]) => `经 ${r} ${fmtBytes(b)}`),
                    p.tunnel > 0 && `VPN 隧道 ${fmtBytes(p.tunnel)}（与经 VPN 的流量重复，不计入总量）`,
                  ]
                    .filter(Boolean)
                    .join(' · ')}
                </div>
                {p.topHosts.length > 0 && (
                  <div className="host-list">
                    {p.topHosts.map(([h, b]) => (
                      <div key={h} className="host-row">
                        <span className="host-name" title={h}>
                          {h}
                        </span>
                        <span className="host-bytes">{fmtBytes(b)}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
