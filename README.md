# AppTraffic

Windows / macOS 托盘（菜单栏）小工具：按应用统计上网流量，并区分**直连 / 经代理 / 经 VPN**。点击托盘图标弹出详情，悬停显示今日总量和实时速度。

（前身是只统计 Clash Verge 流量的 ClashTraffic。）

## 功能

- 按进程累计上传 / 下载；今天 / 7 天 / 30 天 / 全部
- 每个应用的流量拆成路线：直连、经某个代理客户端（Clash Verge、Mihomo Party、v2rayN…）、经某个 VPN 网卡
- 实时速度（总体 + 每个应用）
- 点开某个应用：exe 路径（点击在资源管理器中定位）、连接数、访问最多的域名
- 右键菜单：开机自启、打开数据目录、退出

## 数据源

| 数据源 | 作用 | 需要 |
|---|---|---|
| **系统流量**（Windows：ETW；macOS：nettop） | 所有应用的真实收发字节、对端主机、所走网卡 | Windows 需管理员授权（界面上点「启用」）；macOS 无需授权，默认开启 |
| **代理**（Clash 兼容 API） | 每条连接走了哪个节点 / DIRECT、目标域名 | 自动发现，无需配置 |

两者都可单独工作；同时可用时互相对账，每个字节只记一次。

### 系统流量（ETW）

`traffic-etw.exe`（Rust）以管理员身份订阅 Microsoft-Windows-Kernel-Network（TCP/UDP 收发）、DNS-Client（把 IP 还原成域名）和 Kernel-Process（进程启动时记下 exe 路径，短命进程也能归属），每秒把各进程的增量通过命名管道 `\\.\pipe\apptraffic-etw` 推给托盘程序。托盘程序断开 30 秒后它自行退出。

- 回环流量（应用 → 本机代理端口）不算上网流量，但会上报「谁连了哪个本地端口」，用来把代理的出网流量归还给应用
- 按本机地址判断流量走的网卡；VPN 网卡上的流量记为「VPN · 网卡名」，VPN 客户端自身的加密外层流量记为「VPN 隧道」，不计入总量

**启用方式**：第一次点「启用」弹一次 UAC，注册计划任务 `\AppTraffic\ETW Collector`（最高权限、仅当前用户可运行），之后启动 AppTraffic 时静默拉起采集程序。点「停用」或卸载时删除该任务。计划任务只会指向 Program Files 里的 `traffic-etw.exe`（普通用户无法替换）；装在别处时退回为每次启用都弹 UAC。

### 系统流量（macOS：nettop）

常驻一个 `nettop -L 0 -s 1 -d`（系统自带，普通权限），每秒读各进程的收发增量和当前连接（含网卡）：

- 连到 `lo0` 上另一进程监听端口的连接记为回环流量（同 Windows，用来归还代理流量）
- 两次采样之间已关闭的连接没有明细，按该进程上一次的连接构成（回环 / 各网卡）分摊
- 有可路由地址的 `utun` / `ppp` / `ipsec` 网卡算 VPN；只连着一个 VPN 服务时用它在「网络」设置里的名字。Clash TUN（198.18.0.0/15）不算 VPN
- `.app` 包里的进程（`Google Chrome Helper` 等）归到包名下

### 代理（Clash 兼容 API）

从正在运行的内核进程（mihomo / Clash Meta / sing-box）的命令行和配置文件里找出控制接口（命名管道或 HTTP + secret），用 `/version` 确认；客户端名称取自启动内核的父进程。`/connections` 每秒轮询一次，按连接累计差值。

- 系统代理 / SOCKS：应用连到内核端口的回环字节（ETW）定量，Clash 给出代理与直连的比例和域名
- TUN：应用在 TUN 网卡上的字节（ETW）定量，Clash 给出比例；内核自身的出网流量剔除
- Clash 未开进程识别（`find-process-mode: off`，macOS 上常见）时，未识别的代理连接按回环流量归给最近连过内核端口的本机应用；没有这样的应用时视为局域网设备
- 没有 ETW 时退回纯 Clash 统计，两次轮询之间的短连接记为 `(短连接/未归属)`
- 没有 Clash API 的本地代理（如 xray）：按各应用连到系统代理端口的回环流量，分摊代理进程的出网流量

## 数据

`%APPDATA%\app-traffic\traffic.json`（macOS：`~/Library/Application Support/app-traffic/traffic.json`），按 `日期 → 进程 → 路线` 记录，保留 90 天。首次启动会复制旧版 ClashTraffic 的数据（`%APPDATA%\clash-traffic`，原文件不动）。

## 限制

- 只统计 AppTraffic 运行期间的流量，建议开启「开机自启」
- WireGuard 内核驱动的外层流量可能记在 System 名下，不会被识别为 VPN 隧道
- 不在内置名单里的 VPN 客户端，其外层流量会与应用流量重复计入
- macOS：Safari 等 WebKit 应用的流量都记在 `com.apple.WebKit.Networking` 名下；没有域名，只有对端 IP

## 平台

和操作系统相关的部分都在 `src/main/platform/`，其余代码只通过 `Platform` 接口（`platform/types.ts`）使用：

| | Windows（`win32/`） | macOS（`darwin/`） |
|---|---|---|
| 查内核进程（父进程、命令行） | WMI（PowerShell） | `ps`（按 `-flag` 切分参数，路径可含空格） |
| 已知客户端配置 / 本地 socket | Clash Verge 配置、`\\.\pipe\*` | Clash Verge 配置、`/tmp`、`/tmp/verge` 下的 `.sock` |
| 系统代理端口 | 注册表 Internet Settings | `scutil --proxy` |
| 系统流量 | ETW 助手 `traffic-etw.exe` + 计划任务 | `nettop`（无需 root） |

## 开发

```bash
npm install
npm run dev            # 开发模式
npm run typecheck
npm run build:native   # 编译 traffic-etw.exe（需要 Rust）
npm run dist           # 编译助手 + 打包 NSIS 安装包
npm run dist:mac       # 在 Mac 上打包 dmg（x64 + arm64，未签名）
# 安装包按版本号收集到 dist/<版本号>/（中间文件在 dist/.build/）；Mac 上打的 dmg 拷到同一目录
node scripts/gen-icon.js   # 重新生成图标

native/target/release/traffic-etw.exe --list-ifaces out.json   # 查看网卡及 VPN 判定（无需管理员）
```

## 包体大小

安装包大，是因为 Electron 每个安装包都自带一整套 Chromium 内核和 Node.js，AppTraffic 自己的代码只占很小一部分。

| 包 | 大小 |
|---|---|
| Windows 安装包（NSIS，已压缩） | 约 68 MB |
| Windows 安装后 | 约 221 MB |
| Mac dmg（arm64 / x64 各一个） | 约 81 / 88 MB |

Windows 安装后的构成（1.0.2 实测）：

| 内容 | 大小 | 说明 |
|---|---|---|
| `AppTraffic.exe` | 172.5 MB | 主程序，编译进了 Chromium、V8 和 Node.js |
| Chromium 附带文件 | 约 45 MB | 渲染库（`libGLESv2`、`vk_swiftshader`、`d3dcompiler_47`）、`icudtl.dat`、`resources.pak`、`ffmpeg.dll`、许可证等 |
| `resources/app.asar` | 0.3 MB | AppTraffic 的界面和业务代码 |
| `resources/traffic-etw.exe` | 0.5 MB | ETW 采集程序（Rust） |

99% 以上是 Electron 运行环境本身，和功能多少无关。

已经做的瘦身：

- `electronLanguages` 只保留 `zh-CN` 和 `en-US` 两个语言包（Chromium 默认带 50 多种语言）。Mac 上由 `scripts/after-pack.js` 删掉其余的 `.lproj`，再重新签名
- 不引入大型第三方依赖，只用 Node 和 Electron 自带的能力

想要再明显变小，只能换框架：改用 Tauri（Rust 加系统自带的 WebView），安装包大约 5–10 MB。但主进程要全部用 Rust 重写，Windows 依赖 WebView2，Mac 用 WebKit，可能有兼容问题。目前不打算迁移。
