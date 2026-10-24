import { useEffect, useState, useCallback, useRef } from 'react';
import type { Range, Snapshot } from '../../shared/types';
import { fmtBytes, fmtSpeed } from '../../shared/format';
import ProcessList from './components/ProcessList';

const RANGES: { key: Range; label: string }[] = [
  { key: 'today', label: '今天' },
  { key: '7d', label: '7 天' },
  { key: '30d', label: '30 天' },
  { key: 'all', label: '全部' },
];

export default function App() {
  const [range, setRange] = useState<Range>('today');
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const appRef = useRef<HTMLDivElement>(null);

  const load = useCallback(async () => {
    setSnap(await window.electronAPI.getSnapshot(range));
  }, [range]);

  useEffect(() => {
    load();
    window.addEventListener('focus', load);
    return () => window.removeEventListener('focus', load);
  }, [load]);

  useEffect(() => window.electronAPI.onSnapshot(setSnap), []);

  // Keep the window height matched to the content
  useEffect(() => {
    const el = appRef.current;
    if (!el) return;
    const report = () => window.electronAPI.resizePopup(el.offsetHeight + 2);
    const ro = new ResizeObserver(report);
    ro.observe(el);
    report();
    return () => ro.disconnect();
  }, []);

  const sources = snap?.sources ?? [];
  const anyOn = sources.some((s) => s.state === 'on');
  const total = snap ? snap.totalUp + snap.totalDown : 0;
  const routes = snap?.routes ?? [];

  return (
    <div className="app">
      <div className="app-inner" ref={appRef}>
        <header className="app-header">
          <span className="app-title">
            <span className={`status-dot ${anyOn ? 'ok' : 'bad'}`} />
            应用流量
          </span>
          <button className="close-btn" title="关闭" onClick={() => window.electronAPI.closePopup()}>
            ×
          </button>
        </header>

        <div className="source-list">
          {sources.map((s) => (
            <div key={s.id} className={`source-row ${s.state}`}>
              <span className="source-dot" />
              <span className="source-label">{s.label}</span>
              <span className="source-detail" title={s.detail}>
                {s.detail}
              </span>
              {s.action && (
                <button className="source-btn" onClick={() => window.electronAPI.runSourceAction(s.id)}>
                  {s.action}
                </button>
              )}
            </div>
          ))}
        </div>

        <div className="range-tabs">
          {RANGES.map((r) => (
            <button
              key={r.key}
              className={`range-tab ${range === r.key ? 'active' : ''}`}
              onClick={() => setRange(r.key)}
            >
              {r.label}
            </button>
          ))}
        </div>

        <div className="stats-grid">
          <StatCell label="下载" value={fmtBytes(snap?.totalDown ?? 0)} />
          <StatCell label="上传" value={fmtBytes(snap?.totalUp ?? 0)} />
          <StatCell
            label="走代理"
            value={total && routes.length ? `${Math.round(((total - (snap?.totalDirect ?? 0)) / total) * 100)}%` : '—'}
          />
        </div>
        {routes.length > 0 && (
          <div className="route-row">
            {routes.map(([r, b]) => (
              <span key={r}>
                经 {r} {fmtBytes(b)}
              </span>
            ))}
          </div>
        )}
        <div className="speed-row">
          <span>实时</span>
          <span className="speed-val">↓ {fmtSpeed(snap?.speedDown ?? 0)}</span>
          <span className="speed-val">↑ {fmtSpeed(snap?.speedUp ?? 0)}</span>
        </div>

        <ProcessList procs={snap?.procs ?? []} />

        <footer className="app-footer">
          <span>
            {snap?.since ? `记录自 ${snap.since}` : '暂无记录'}
          </span>
          <button className="link-btn" onClick={() => window.electronAPI.openDataFolder()}>
            数据目录
          </button>
        </footer>
      </div>
    </div>
  );
}

function StatCell({ label, value }: { label: string; value: string }) {
  return (
    <div className="stat-cell">
      <span className="stat-value">{value}</span>
      <span className="stat-label">{label}</span>
    </div>
  );
}
