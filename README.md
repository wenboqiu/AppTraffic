# ClashTraffic

Windows 托盘小工具：按应用统计经过 Clash Verge (Rev) 的流量。点击托盘图标弹出详情，悬停显示今日总量和实时速度。

## 功能

- 按进程累计上传 / 下载，区分走代理和直连
- 今天 / 7 天 / 30 天 / 全部 切换
- 实时速度（总体 + 每个应用）
- 点开某个应用：exe 路径（点击在资源管理器中定位）、累计连接数、访问最多的域名
- 右键菜单：开机自启、打开数据目录、退出

## 原理

Clash Verge Rev 的内核 mihomo 会在命名管道 `\\.\pipe\verge-mihomo` 上提供 REST API。`/connections` 返回当前所有连接，每条带进程名（`find-process-mode`）、目标域名、出站链路和该连接累计的字节数。

本工具每秒轮询一次，记住每个连接上一次的读数，只累加差值，按 `日期 → 进程` 写入
`%APPDATA%\clash-traffic\traffic.json`，保留 90 天。

- 两次轮询之间就开就关的短连接，以及连接关闭前最后不到 1 秒的流量，会用内核的全局计数补差，记为 `(短连接/未归属)`
- 内核自己发起的连接（DNS 上游等）记为 `(Clash 内核 · DNS 等)`
- 启动或重连 Clash 后的第一次读数只作为基线，不会把已有连接的历史流量重复计入

如果命名管道连接不上，会退回到 `clash-verge.yaml` 里配置的 `external-controller`（HTTP + secret）。

## 限制

- 只统计经过 Clash 的流量。没开 TUN 时，不走系统代理的程序看不到
- 只统计本工具运行期间的流量，建议开启「开机自启」
- 进程识别依赖内核的 `find-process-mode`（Clash Verge Rev 默认开启）

## 开发

```bash
npm install
npm run dev        # 开发模式
npm run typecheck
npm run dist       # 打包 NSIS 安装包到 dist/
node scripts/gen-icon.js   # 重新生成图标
```
