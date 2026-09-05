# 跨平台发布指南

## 发行模型

Mobius 使用同一套 React、TypeScript 和 Rust 源码，但在原生 GitHub Actions runner 上分别构建：

| Runner | 产物 | 备注 |
| --- | --- | --- |
| Windows | x86_64 NSIS `setup.exe` | 正式公开发布应使用 Authenticode 签名 |
| Ubuntu | x86_64 AppImage、Debian 包 | 需要 WebKitGTK 和打包工具 |
| macOS | arm64 DMG | Apple Silicon |
| macOS | x86_64 DMG | 在 macOS runner 上安装对应 Rust target 后构建 |

这叫“一套源码、多平台产物”，不是一个文件跨系统运行。每个安装包都会携带对应架构的 ADB、scrcpy/Server、最小 FFmpeg、AAPT2、Detect It Easy 3.21 CLI/规则库、go-ios 与 Mobius SSH/SFTP 资源；macOS `.app` 本身仍是 Bundle 目录，Windows 安装器依赖 WebView2，Linux 包也需要与发行版运行时和设备权限配合。

## 工作流

- `.github/workflows/ci.yml`：推送和 Pull Request 时，在 Windows、Linux、macOS 上执行前端构建、Rust 格式检查、`cargo check`、`cargo clippy -D warnings` 和 `cargo test`。
- `.github/workflows/release.yml`：推送与应用版本匹配的 `v*` 标签，或者在 Actions 中手动触发。它先校验三处版本、创建草稿 Release，再由四个原生 runner 生成并验证目标工具集、构建安装包，同时生成第三方源码/构建脚本归档。所有平台成功后才会生成 `SHA256SUMS.txt` 并按触发方式决定是否公开发布。每个原生包也会保留为 Actions Artifact。

工作流只接受 `v<version>` 形式的精确标签，例如应用版本为 `0.3.0` 时标签必须是 `v0.3.0`。它会在任何编译开始前确认 `package.json`、`src-tauri/Cargo.toml` 和 `src-tauri/tauri.conf.json` 三处版本完全一致，并拒绝非 SemVer 版本或不匹配标签。

### 触发发布

通过标签触发会在四个平台构建全部成功后自动公开 Release：

```bash
git tag v0.3.0
git push origin v0.3.0
```

手动触发时，`release_tag` 可以留空，工作流会根据 Tauri 版本生成 `v<version>`。`publish` 默认关闭，因此适合先产出草稿、下载验收后再在 GitHub 页面公开；只有明确开启 `publish` 才会在构建成功后公开。对同一标签重新运行会复用已有 Release，不会并发创建重复发布。

标准产物包括：

- Windows x64：NSIS `setup.exe`
- Linux x64：AppImage 和 Debian `deb`
- macOS Apple Silicon：arm64 DMG
- macOS Intel：x86_64 DMG
- `Mobius-Device-Lab_<version>_third-party-sources.tar.xz`：固定 scrcpy 及便携依赖、FFmpeg、Detect It Easy 与目标所用 Qt 模块、go-ios/Go、Mobius SSH 模块上游源码、第一方 helper 源码/测试、公开补丁、工具锁与完整构建/验证脚本
- `SHA256SUMS.txt`：本次所有原生包与第三方源码归档的 SHA-256

## 随包工具生成与合规

[`packaging/toolchain.lock.json`](../packaging/toolchain.lock.json) 是工具供应链的唯一版本入口。目前固定：

| 组件 | 版本与来源 | 打包方式 |
| --- | --- | --- |
| ADB | 37.0.0，scrcpy 4.1 官方便携发行包 | 与 Google Platform Tools 37.0.0 目标归档逐字节比对，带对应 `NOTICE`；Windows 保留匹配 DLL |
| scrcpy / Server | 4.1，Genymobile 官方便携发行包 | 客户端与 `scrcpy-server` 必须来自同一归档 |
| scrcpy 便携依赖 | FFmpeg 8.1.2、SDL 3.4.12、libusb 1.0.30、dav1d 1.5.3、目标对应 zlib；Windows 另含 MinGW-w64 11.0.1 runtime | 保留完整源码、原始许可/第三方声明、链接方式和 LGPL 重链指南 |
| FFmpeg | 9.0.1，FFmpeg 官方源码 | 原生 runner 最小 LGPL 构建；禁用 GPL、nonfree、网络与外部编解码器；Windows 应用公开原生计时补丁并锁定编译器/审计 DLL 导入 |
| AAPT2 | 9.4.0-15978811，Google Maven | 提取原生二进制与完整 `NOTICE` |
| Detect It Easy | 3.21，horsicq/DIE-engine 官方目标归档和源码 | 随包仅使用控制台 `diec`、APK/DEX 规则库与所需目标运行库；Linux 从锁定 Ubuntu 20.04 包携带 Qt 所需的 ICU/zlib/PCRE/PCRE2/double-conversion/GLib 完整非 glibc 闭包；DIE 本体为 MIT，动态库按各自许可交付可替换说明、精确源码和 Ubuntu 打包变更 |
| go-ios | 1.3.2-mobius.1，go-ios 1.3.2 固定源码 | 用 Go 1.26.5、CGO 关闭的原生构建；应用公开补丁以标识版本并将转发/可选截图监听绑定 `127.0.0.1` |
| Mobius SSH/SFTP | 0.2.0，仓库内第一方源码 | 用 Go 1.26.5、CGO 关闭构建；锁定 x/crypto/ssh、pkg/sftp 及实际链接模块，四平台随包作为 `ssh`/`scp` |

每个平台的准备任务先验证 HTTPS 归档的精确字节数与 SHA-256，再执行有条目类型、单文件和总展开容量限制的安全解包。scrcpy 和 Detect It Easy 源码归档只按锁定路径提取普通许可文件，不跟随源码归档中的符号链接；FFmpeg/go-ios/Mobius SSH 仅从锁定源码与模块构建。随后为目标目录生成逐文件 `manifest.json`，记录组件、许可证、大小、SHA-256 和可执行位；四个原生目标分别检查文件集合、目标架构、版本命令、scrcpy Server 完整性，以及 Detect It Easy `diec`/规则库/Qt 运行库/许可材料，并用随包数据库执行原生 JSON 扫描冒烟。Linux 还会直接解析所有随包 ELF 的 `DT_NEEDED` 和 `DT_SONAME`，对照锁定图确认每个非基础系统库均在包内且没有多余库。生成 AppImage 时，工作流先让 `linuxdeploy` 只规范化应用骨架，再把已经闭包验证的目标工具目录连同原始清单原样注入 AppDir 并直接验证后重打包；这样不会让打包器把工具目录里的 Qt/GLib 等私有运行库误当成应用自身依赖并改写，同时任何注入损坏都会造成清单验证失败。最终步骤会分别解开 DEB 和 AppImage，再运行同一套架构、清单、ELF 闭包与原生冒烟验证，通过后才上传最终 AppImage。macOS 应用本体的构建下限固定为 12.0（保留调用者显式设置的更低值），嵌套 Mach-O 签名会在 Tauri 打包前完成，签名改变文件后刷新并再次验证清单。官方 DIE 3.21 arm64 产物的最低版本是 macOS 13.0，因此 macOS 12 Apple Silicon 上应用可运行，但 APK 加固扫描不作可用性承诺，引擎无法运行时必须返回“无法确定”。

许可证、目标对应 ADB/AAPT2 NOTICE、scrcpy 便携依赖及内嵌第三方声明、FFmpeg 配置、Detect It Easy/内嵌依赖许可、Qt attribution 引用的许可证/版权正文及模块 `LICENSES/` 目录、Qt 动态重链说明、Go 许可和 go-ios/Mobius SSH 模块清单与依赖许可随平台工具目录进入安装包。独立 source archive 保存这些组件的精确完整源码、链接元数据、许可、重建脚本与逐文件校验和，并与安装器共同进入 `SHA256SUMS.txt`。这套机制不等同于完整 SBOM、恶意软件扫描或平台发布签名；这些仍是正式发布门禁。

Frida Server 不属于随包工具。它始终由用户按测试设备 ABI 与所需版本上传，项目也没有主机 Frida CLI 运行依赖。SSH/SFTP 客户端已随包；Windows Apple Mobile Device 驱动/服务、Linux usbmuxd/udev、设备信任、Developer Disk Image、越狱 SSH Server/AppSync 和设备端安装器仍是系统或测试机条件。

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
3. 审核 `packaging/toolchain.lock.json`、go-ios 补丁、Detect It Easy/Qt 来源与重链说明、第三方声明与构建脚本的全部变更；逐项复核版本、上游 HTTPS 来源、目标架构、大小、SHA-256、许可证、NOTICE、源码提供义务和运行时依赖。不要手工把未锁定文件放入 `resources/tools`。
4. 面向高保证发布时，把 GitHub Actions 的浮动主版本标签固定到已审计的完整提交 SHA。
5. 在 CI 三个平台上通过前端构建、Rust 格式、check、clippy 和 test。
6. 确认 Windows x64、Linux x64、macOS arm64 和 macOS x64 四个 runner 的工具准备/逐文件清单/架构/版本冒烟测试全部通过；每个目标都必须通过 `diec` 原生架构、版本、规则库、扫描冒烟、Qt 运行库和随包许可材料验证；Linux 还必须通过逐 ELF 精确 NEEDED 闭包检查。确认第三方源码归档包含相应 Ubuntu `.dsc`/打包补丁、存在且在总校验和中，再对实际安装包执行恶意软件扫描、SBOM 生成和签名验签。
7. 在干净 Windows、Linux、Intel Mac 和 Apple Silicon Mac 上安装、首次启动、升级和卸载。
8. 使用 Android USB/Wi-Fi/模拟器验证工具链默认解析到随包 ADB 37.0.0、scrcpy/Server 4.1、FFmpeg 9.0.1、AAPT2 和 Detect It Easy 3.21，冷启动固定进入工作台，连接后默认是内嵌 scrcpy Server 连续视频，而非默认单帧轮询或自动打开独立窗口。确认手机外框始终为紧凑纵向比例，横屏内容在框内完整适配且页面不跳变；首屏无需滚动即可看到屏幕操作、连接入口和底部工具链健康摘要。再用真实启流验证 Server 文件与客户端版本匹配；故意破坏依赖/链路时应降级为低频单帧。验证暂停、重连、切换设备、离开页面和退出都会清理受管 Server/FFmpeg、回环 reverse 与临时 jar；这一条只针对投屏流。录屏须超过 20 秒仍继续，切换设备和离开工作台后仍必须保持同一录制会话；只有用户显式点击停止或退出整个应用时才收尾，并验证 MP4 可播放、文件已保存且设备临时文件已清理。对真实 APK 验证随包 `diec` 扫描最长 25 秒，并分别覆盖“发现特征”、完整扫描零命中的“未发现已知特征”，以及引擎/数据库缺失、超时、截断、非零退出或无效 JSON 对应的“无法确定”。再覆盖独立 scrcpy 交互窗口、截图剪贴板/保存、APK 安装，以及应用列表行内明显的 base/split APK 导出按钮；“更多”只能保留清数据和卸载，并继续验证目标锁定与系统应用保护。最后验证用户上传 Frida Server 的自定义端口 forward。
9. 使用获准的越狱 iOS 真机验证随包 go-ios 1.3.2-mobius.1 的 USB 与已配对网络设备去重枚举，逐个点击设备信息、只读配对状态、应用列表和限时日志采样，确认所有通道锁定精确 UDID、采样到期不留驻子进程。验证 go-ios screenshot 在匹配 Developer Disk Image 下的默认采样预览、暂停/恢复、剪贴板/电脑截图，以及仅 SSH 端点的诚实降级提示。同时验证 IPA 解析，go-ios USB 安装与 SSH 设备安装器的自动选路，用户/系统 App 清单、`.app` 分析归档及临时文件清理，USB 转发与私网 LAN SSH、随包 Mobius SSH/SFTP 的密码/私钥认证、允许目录文件边界、首次/变更主机密钥、空格与单引号路径、Frida 回环隧道，以及概览/进程/固定路径工具/syslog 四类 SSH 诊断。另在安装 libimobiledevice/`iproxy` 的隔离环境中分别制造 go-ios 单项失败，验证兼容回退绑定同一 UDID 且不会掩盖双重失败。Respring/重启只在可恢复的专用测试机上验证实际 SSH 目标展示、30 秒单次票据与过期/重放拒绝。
10. 在 iOS `网络` 页分别验证随包 go-ios 的 USB 本机→设备、`iproxy` 兼容回退、SSH `-L` 的本机→设备与 SSH `-R` 的设备→本机。使用监听检查确认 go-ios 与 `iproxy` 的主机端都只有 `127.0.0.1`，SSH 转发规格的两端也均为 `127.0.0.1`；USB 反向请求被拒绝，且不可用本机端口不会被覆盖。再替换为上游未补丁或版本输出异常的 go-ios，确认它不能进入 USB 转发路径。停止单条隧道时不影响外部 go-ios/`iproxy`/`ssh`；关闭 SSH 会话时清理该会话的 `-L`/`-R`，独立 USB 转发保持到手动停止或应用退出，应用退出后不应留下任何 Mobius 受管 iOS 隧道。
11. 验证 Reverse 与 Android 系统代理始终分开：只有显式按钮才写系统代理。在无代理和既有静态代理上分别验证五字段快照恢复、`:0` 清理内存/变更广播和退出清理；在任一字段被外部程序改动时确认 Mobius 不覆盖新值，在现有 PAC/非空排除列表上确认它在任何写入前拒绝操作。
12. 核对应用未把内嵌屏幕令牌、配对码、SSH 密码、证书、私钥、用户目录、设备 UDID 或测试数据写入公开日志或本地偏好设置。
13. 确认 Release 中的说明没有把未完成能力表述为已交付，且实际功能严格符合设备开发、调试与管理白名单。

> 真机验收必须保存实际 UDID 被随包 `ios list --details` 枚举、固定操作结果和隧道端到端连通的内部证据；兼容回退测试另保存 `idevice_id -l/-n` 结果。未实际返回设备的环境只能记录代码、构建和单元测试结果，**不能记为 iOS 真机验收通过**。

## 本地构建与 CI 的关系

本地可以运行当前操作系统的开发包：

```bash
pnpm install --frozen-lockfile
python3 scripts/prepare_tool_bundle.py \
  --target macos-aarch64 \
  --output src-tauri/resources/tools/macos-aarch64
python3 scripts/verify_tool_bundle.py \
  --target macos-aarch64 \
  --root src-tauri/resources/tools
pnpm run build
pnpm run tauri build
```

示例目标为 Apple Silicon；必须替换为当前原生目标。Windows 在发布工作流所用的 MSYS2 MINGW64 环境中运行对应 `python` 命令。准备脚本会联网下载锁定输入，准备 Detect It Easy CLI/规则库/必要运行库，并从源码构建 FFmpeg/go-ios/Mobius SSH；安装后的应用自身不会下载工具。

不要把从 macOS 交叉生成的 Windows/Linux 文件当作正式发布结果。安装器格式、系统 WebView、驱动、原生依赖、代码签名和启动行为都必须在目标操作系统上验证。
