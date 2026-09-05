# 跨平台发布指南

## 发行模型

Mobius 使用同一套 React、TypeScript 和 Rust 源码，但在原生 GitHub Actions runner 上分别构建：

| Runner | 产物 | 备注 |
| --- | --- | --- |
| Windows | x86_64 NSIS `setup.exe` | 正式公开发布应使用 Authenticode 签名 |
| Ubuntu | x86_64 AppImage、Debian 包 | 需要 WebKitGTK 和打包工具 |
| macOS | arm64 DMG | Apple Silicon |
| macOS | x86_64 DMG | 在 macOS runner 上安装对应 Rust target 后构建 |

这叫“一套源码、多平台产物”，不是一个文件跨系统运行。macOS `.app` 本身是 Bundle 目录，Windows 安装器依赖 WebView2，Linux 包也需要与发行版运行时和设备权限配合。

## 工作流

- `.github/workflows/ci.yml`：推送和 Pull Request 时，在 Windows、Linux、macOS 上执行前端构建、Rust 格式检查、`cargo check`、`cargo clippy -D warnings` 和 `cargo test`。
- `.github/workflows/release.yml`：推送与应用版本匹配的 `v*` 标签，或者在 Actions 中手动触发。它先校验三处版本、创建草稿 Release，再由四个原生 runner 上传安装包。所有平台成功后才会生成 `SHA256SUMS.txt` 并按触发方式决定是否公开发布。每个原生包也会保留为 Actions Artifact。

工作流只接受 `v<version>` 形式的精确标签，例如应用版本为 `0.1.0` 时标签必须是 `v0.1.0`。它会在任何编译开始前确认 `package.json`、`src-tauri/Cargo.toml` 和 `src-tauri/tauri.conf.json` 三处版本完全一致，并拒绝非 SemVer 版本或不匹配标签。

### 触发发布

通过标签触发会在四个平台构建全部成功后自动公开 Release：

```bash
git tag v0.1.0
git push origin v0.1.0
```

手动触发时，`release_tag` 可以留空，工作流会根据 Tauri 版本生成 `v<version>`。`publish` 默认关闭，因此适合先产出草稿、下载验收后再在 GitHub 页面公开；只有明确开启 `publish` 才会在构建成功后公开。对同一标签重新运行会复用已有 Release，不会并发创建重复发布。

标准产物包括：

- Windows x64：NSIS `setup.exe`
- Linux x64：AppImage 和 Debian `deb`
- macOS Apple Silicon：arm64 DMG
- macOS Intel：x86_64 DMG
- `SHA256SUMS.txt`：本次所有 Actions Artifact 中原生包的 SHA-256

若组织策略将 `GITHUB_TOKEN` 限制为只读，需在仓库的 Actions 设置中允许工作流申请 `contents: write`；工作流只在准备、上传和最终发布任务中申请该权限。

## 可选的签名 Secrets

构建未签名的测试包不需要自定义 secrets。公开下载前应配置目标平台的正式签名，并验证签名和公证步骤确实执行。

### Tauri 更新签名（当前未启用）

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

只有在应用接入 Tauri Updater、公钥和更新端点后，这两个变量才会生成可验证的更新签名。它们不等同于 Windows Authenticode 或 Apple Developer ID。当前发布工作流显式关闭 updater JSON 和 updater 签名上传，因此现阶段不需要配置这两个 secret。

### macOS Developer ID 与公证

- `APPLE_CERTIFICATE`：Base64 编码的 `.p12`。
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`：建议使用 App-Specific Password。
- `APPLE_TEAM_ID`

也可以改用 App Store Connect API Key；若采用该模式，应遵循当前 Tauri 官方文档替换为对应 secrets，避免同时维护两套公证凭据。没有配置 `APPLE_SIGNING_IDENTITY` 时，工作流会对 macOS 包使用 ad-hoc 签名，以保证 Bundle 结构完整；这不是 Developer ID 签名或公证，公开分发仍会触发 Gatekeeper 的未识别开发者提示。

### Windows 代码签名

若选用 Azure Artifact Signing，通常需要下列仓库 secrets：

- `AZURE_CLIENT_ID`
- `AZURE_CLIENT_SECRET`
- `AZURE_TENANT_ID`
- `AZURE_ARTIFACT_SIGNING_ENDPOINT`
- `AZURE_ARTIFACT_SIGNING_ACCOUNT`
- `AZURE_CERTIFICATE_PROFILE`

当前工作流没有将这些值传入一个虚假的签名步骤，因为仅设置 secrets 不会自动签名。还需要在 Windows 专用 Tauri 配置中配置 `bundle.windows.signCommand`，并在 runner 安装官方签名 CLI。也可以改用组织已有的 EV/OV 证书和 `signtool`。配置生效后，必须对应用可执行文件和最终 NSIS 安装器都做验签。

### Linux 包签名

- `LINUX_GPG_PRIVATE_KEY`
- `LINUX_GPG_PASSPHRASE`

当前工作流只生成 AppImage/Debian 构建产物，没有自动导入 GPG 私钥，上述两个 secrets 因此尚未使用。若通过 APT/RPM 仓库分发，应在独立发布步骤对仓库元数据和包进行签名；不要把私钥写进仓库或普通构建日志。

## 发布前检查

1. 更新版本、变更日志和第三方许可证清单。
2. 锁定 pnpm 与 Cargo 依赖，并审核依赖变更。
3. 若发行包要加入 `resources/tools` 中的第三方二进制（包括 `scrcpy-server` 或 `ffmpeg`），逐项固定版本、上游下载来源、目标架构和 SHA-256，并完成许可证、NOTICE/SBOM、依赖扫描与再分发审查；当前源码默认不附带任何这类二进制。不要仅因为内嵌屏幕依赖它们就放宽此边界。
4. 面向高保证发布时，把 GitHub Actions 的浮动主版本标签固定到已审计的完整提交 SHA。
5. 在 CI 三个平台上通过前端构建、Rust 格式、check、clippy 和 test。
6. 对实际安装包执行恶意软件扫描、SBOM 生成和签名验签。
7. 在干净 Windows、Linux、Intel Mac 和 Apple Silicon Mac 上安装、首次启动、升级和卸载。
8. 使用 Android USB/Wi-Fi/模拟器验证冷启动固定进入工作台，连接后默认是内嵌 scrcpy Server 连续视频，而非默认单帧轮询或自动打开独立窗口。确认手机外框始终为紧凑纵向比例，横屏内容在框内完整适配且页面不跳变；首屏无需滚动即可看到屏幕操作、连接入口和底部工具链健康摘要。再用真实启流验证 Server 文件与客户端版本匹配；缺失依赖/链路中断时应降级为低频单帧。验证暂停、重连、切换设备、离开页面和退出都会清理受管 Server/FFmpeg、回环 reverse 与临时 jar。录屏须超过 20 秒仍继续，点击停止后 MP4 可播放，切设备、离页或退出会自动停止、完成封装、保存并清理设备临时文件。再覆盖独立 scrcpy 交互窗口、截图剪贴板/保存、APK 安装/导出、应用启动/停止/清数据/卸载的目标锁定与系统应用保护，以及 Frida 自定义端口 forward。
9. 使用获准的越狱 iOS 真机验证 USB 与已配对网络设备去重枚举，并确认工具链单独显示 `idevicepair` 和 `idevicesyslog` 状态。逐个点击 `ideviceinfo`设备信息、`idevicepair validate`配对验证、`ideviceinstaller list --all`应用列表和限时 `idevicesyslog`日志采样，确认 USB/网络通道锁定精确 UDID、网络设备正确使用 `-n`、采样到期不留驻子进程。验证 `idevicescreenshot` 在匹配 Developer Disk Image 下的默认采样预览、暂停/恢复、剪贴板/电脑截图，以及仅 SSH 端点的诚实降级提示。同时验证 IPA 解析，USB `ideviceinstaller` 与 SSH 设备安装器的自动选路，用户/系统 App 清单、`.app` 分析归档及临时文件清理，USB `iproxy` 与私网 LAN SSH、密码/私钥认证、允许目录文件边界、Frida 回环隧道，以及概览/进程/固定路径工具/syslog 四类 SSH 诊断。另验证 Respring/重启的实际 SSH 目标展示、30 秒单次票据、过期/重放拒绝；设备动作只在可恢复的专用测试机上单独执行。
10. 在 iOS `网络` 页分别验证 USB `iproxy` 的本机→设备、SSH `-L` 的本机→设备与 SSH `-R` 的设备→本机。确认 `iproxy` 的主机监听仅在 `127.0.0.1`、SSH 转发规格的两端均为 `127.0.0.1`、`iproxy` 反向请求被拒绝，且不可用本机端口不会被覆盖。停止单条隧道时不影响外部 `iproxy` / `ssh`；关闭 SSH 会话时清理该会话的 `-L` / `-R`，独立 USB `iproxy` 保持到手动停止或应用退出，应用退出后不应留下任何 Mobius 受管 iOS 隧道。
11. 验证 Reverse 与 Android 系统代理始终分开：只有显式按钮才写系统代理。在无代理和既有静态代理上分别验证五字段快照恢复、`:0` 清理内存/变更广播和退出清理；在任一字段被外部程序改动时确认 Mobius 不覆盖新值，在现有 PAC/非空排除列表上确认它在任何写入前拒绝操作。
12. 核对应用未把内嵌屏幕令牌、配对码、SSH 密码、证书、私钥、用户目录、设备 UDID 或测试数据写入公开日志或本地偏好设置。
13. 确认 Release 中的说明没有把未完成能力表述为已交付，且实际功能严格符合设备开发、调试与管理白名单。

> 真机验收必须保存实际 UDID 被 `idevice_id -l` 或 `idevice_id -n` 枚举、命令结果和隧道端到端连通的内部证据。本轮开发环境中 `idevice_id -l` 未返回设备，因此当前只能记录代码/构建/单元测试结果，**不能记为 iOS 真机验收通过**。

## 本地构建与 CI 的关系

本地可以运行当前操作系统的开发包：

```bash
pnpm install --frozen-lockfile
pnpm run build
pnpm run tauri build
```

不要把从 macOS 交叉生成的 Windows/Linux 文件当作正式发布结果。安装器格式、系统 WebView、驱动、原生依赖、代码签名和启动行为都必须在目标操作系统上验证。
