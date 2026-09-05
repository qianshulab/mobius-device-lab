# 架构说明

## 目标

Mobius Device Lab 采用一套源码、多个原生发行包的结构。界面在三个桌面系统上保持一致，设备操作则由 Rust 核心使用当前操作系统上的受信任命令行工具完成。

```text
React / TypeScript UI
        │  typed Tauri invoke
        ▼
Rust 命令边界 ── 输入校验 ── 会话资源归属
        │
        ├── 包分析: ZIP / plist / Mach-O / aapt2 / apkanalyzer
        ├── Android: adb / scrcpy + scrcpy-server / FFmpeg / 用户提供的 frida-server
        └── iOS: libimobiledevice / ideviceinstaller / idevicesyslog / iproxy / OpenSSH
```

## 目录职责

| 路径 | 职责 |
| --- | --- |
| `src/` | 页面、组件、前端类型、Tauri 调用适配与浏览器预览数据 |
| `src-tauri/src/commands/` | 面向界面的窄命令接口；每项操作都有明确输入结构 |
| `src-tauri/src/commands/ios_host_tools.rs` | 将 `ideviceinfo` / `idevicepair validate` / `ideviceinstaller list --all` / 限时 `idevicesyslog` 约束为四个固定主机诊断 |
| `src-tauri/src/commands/ios_ports.rs` | 创建、列出和停止 USB `iproxy` 与 SSH `-L` / `-R` 回环隧道，并管理其子进程归属 |
| `src-tauri/src/validation.rs` | 设备标识、地址、端口、远端路径和本地路径校验 |
| `src-tauri/src/runner.rs` | 外部进程启动、超时、输出截断、错误归一化和敏感值替换 |
| `src-tauri/src/state.rs` | 当前会话创建的内嵌屏幕流与受管录屏、代理整组快照、Android 映射、Frida 进程、iOS SSH 会话与受管 iOS 隧道子进程归属信息 |
| `src-tauri/src/toolchain.rs` | 工具链配置持久化、来源优先级、可执行文件校验与跨平台解析 |
| `src-tauri/capabilities/` | Tauri 窗口可调用能力的最小授权集合 |
| `.github/workflows/` | 三平台持续集成与原生安装包发布矩阵 |

## 运行模型

1. 前端只能调用已注册的 Tauri 命令，不能提交任意主机程序名。
2. Rust 端先验证设备、网段、端点和路径，再调用固定的外部工具。
3. 常规命令经统一运行器执行，包含硬超时和最多 1 MiB 的单路输出限制。
4. 长期运行的 GUI/Server/转码进程与会话创建的代理/映射由专门命令管理；退出时只清理仍属于本会话、且身份或整组状态仍匹配的资源。
5. 返回值使用结构化成功/错误对象，界面不依赖本地化终端输出判断所有状态。
6. APK/IPA 解析限制文件大小、归档条目数和实际解压字节数；图标和 Mach-O 只读取有界数据。

高级 Android shell 是有意保留的例外：Rust 仍不会启动主机 shell，但提供的字符串会作为一个参数交给 `adb shell`，最终由设备 shell 解释。因此它必须始终被视为专家级高风险入口。

## 当前适配器边界

### Android

- 设备：`adb devices -l`、配对和连接。
- 网络：优先实际 Wi-Fi/以太网、排除隧道和常见虚拟接口；在至多 4 个本机活动 RFC1918 `/24` 中进行有界 TCP 与 ADB 协议候选探测。
- 隧道：ADB forward/reverse 的查看、创建与删除。
- 文件：基于 ADB shell、push、pull。
- 屏幕：连接后在“设备”页首屏默认启动精确 ADB 序列号的 scrcpy Server 连续视频。桌面宽度下将手机画面放在左侧竖向模块，右侧只保留交互、截图与录屏动作，下方紧凑表格负责设备选择；只有缺少完整 scrcpy/FFmpeg 或视频链路失败时才轮询单帧 PNG 降级。需要键鼠交互时显式启动独立 scrcpy 窗口。
- 代理：Reverse 与系统代理分开，只有明确的设置动作才会修改 Android 全局代理。后端记录 `http_proxy`、host、port、排除列表与 PAC URL 的整组原始快照，用于变更检测与恢复。
- 应用：APK 分析、安装、已安装应用枚举、base/split APK 导出，以及经包名和设备绑定的启动、强制停止、清除数据与卸载。清数据/卸载保护系统应用并在界面二次确认。
- 系统观察：确定性的只读命令在当前页执行并显示，重启单独确认；自定义命令进入设备 Shell。
- 动态分析：提供 16.1.4、最新稳定版与自定义版本槽；用户选择匹配 ABI 的本地 `frida-server`，上传为不含 `frida` 的中性远端名；设备/主机端口独立配置并自动创建 forward。

### iOS

- 设备与应用：通过 `idevice_id -l/-n` 合并枚举 USB 与已配对网络设备（USB 优先去重），通过 `ideviceinfo` 读取属性；离线解析 IPA；USB 默认调用 `ideviceinstaller`，Root SSH 会话可探测测试机已有的 `appinst` / `ipainstaller`。
- 主机工具：调试页的 libimobiledevice 分组只向后端发送 `deviceInfo / pairing / apps / syslog` 枚举。后端分别构造 `ideviceinfo`、`idevicepair validate`、`ideviceinstaller list --all` 和 `idevicesyslog --no-colors` 的固定参数，绑定精确 UDID；网络配对通道附加 `-n`。日志以默认 5 秒窗口采样，所有输出均清理控制字符并限长。
- SSH 应用工作流：从固定 iOS 应用目录读取有界 `Info.plist` 元数据；安装临时文件位于会话首个允许根的 `.mobius-runtime`；导出仅生成开发分析 `.app` 归档，不重建 IPA。
- SSH 文件：USB 模式由 `iproxy` 将设备 SSH 端口映射到主机 `127.0.0.1`；LAN 模式只接受私网、回环或链路本地字面 IP。密码模式使用当前可执行文件作为 `SSH_ASKPASS` helper，私钥模式使用 OpenSSH BatchMode；两者都不经过主机 Shell。
- 端口隧道：USB `iproxy` 只实现本机→设备，并将主机监听绑定 `127.0.0.1`；设备服务由 UDID 与 usbmuxd 通道选择，不使用设备 IP。已验证 SSH 会话上的 `-L` 实现本机→设备，`-R` 实现设备→本机；SSH 转发规格的监听端和目标端均为 `127.0.0.1`，参数固定包含 `ExitOnForwardFailure=yes` 和 `GatewayPorts=no`，不暴露 LAN 端口。
- 隧道生命周期：后端保存不可猜测隧道 ID、UDID、可选 `sessionId`、方向、端口和直接子进程。列表时检测并移除已退出子进程；显式停止只处理精确 ID。关闭 SSH 会话清理与该 `sessionId` 绑定的 `-L` / `-R` 隧道；独立 USB `iproxy` 保留到显式停止或应用退出。退出会清理全部仍受管的 iOS 隧道。
- 会话建立时先验证密码或私钥登录并规范化允许目录；浏览、上传、下载、新建和删除都必须留在这些根目录内，拒绝通过符号链接越界，且不能删除允许根本身。
- 当前产品范围只考虑用户拥有或获准测试的越狱设备；设备现有的越狱、AppSync、签名与信任状态均保持不变。
- 屏幕：`idevicescreenshot` 只能面向精确 UDID 的已配对 USB/网络 screenshotr 服务。后端每次重新核对目标、在私有临时目录采集、校验 PNG/尺寸/像素/大小，再内嵌显示或写入剪贴板/用户目录。`ios-ssh:*` 手工端点在调用外部屏幕工具前就会被拒绝。
- iOS Frida 进程由 Root SSH 会话精确归属，设备和主机两端均只绑定回环地址。系统概览、进程、固定路径工具检测和 syslog 使用后端白名单、枚举请求和有界输出；Respring/重启还要求后端短时单次票据与实际 SSH 目标复核。

## 信息架构

侧栏按用户对象而不是底层命令分类：

1. `工作台`：固定启动页，首屏展示 Android 内嵌 scrcpy 连续视频或 iOS 配对 screenshotr 采样画面；使用“紧凑纵向手机画面 + 屏幕操作 + 设备连接”布局。设备完整信息只在二级管理弹窗展示。
2. `应用`：本地 APK/IPA 分析、安装、设备应用与导出。
3. `文件`：远端文件浏览和传输。
4. `网络`：Android 测试代理与 ADB forward/reverse；iOS USB `iproxy` 本机转发与 SSH `-L` / `-R` 双向回环隧道。
5. `调试`：Frida、系统/进程、iOS libimobiledevice 固定主机工具与高级 Shell。
6. `设置`：工具链、网络、文件与媒体、安全和外观。

当前设备始终显示在页面上方的上下文栏，并可在任何主要页面原地切换。工具链只在底部状态栏显示聚合健康状态，点击精确进入“设置 → 工具链”；完整路径、版本与修复入口不再占用工作台。屏幕与连接集中在工作台首屏，代理、应用、文件和调试动作各自归入对应对象页。

## 投屏层级

Android 主画面使用选中 scrcpy 客户端报告的版本号启动对应 Server；发行/安装必须提供与该客户端匹配的 `scrcpy-server` 文件。Server 精确绑定 serial，关闭音频与控制通道，并通过随机 SCID 的 ADB reverse 将原始 H.264 只引到本机回环。FFmpeg 将它转为 MJPEG，Rust 在另一个 `127.0.0.1` 随机端口上，仅向路径、回环 Host、独立 128 位令牌，以及出现时属于允许清单的 Origin 都匹配的单个 WebView 请求提供不缓存视频。画面不经过 Tauri base64 IPC。

每个 Android serial 同时只保留一个受管流和一个受管录屏。显式停止、重建、切换设备、页面销毁、客户端断开或应用退出都会尽力停止 scrcpy Server 与 FFmpeg，关闭 socket，移除该 SCID 的 reverse，并删除本会话生成的设备端 jar；仍在进行的录屏会先正常完成 MP4、保存到预定电脑路径再清理设备临时文件。外层手机窗口始终保持纵向比例；设备横屏时使用 `object-fit: contain` 在纵向窗口内完整显示，页面布局不随旋转跳变。缺少完整 scrcpy/FFmpeg 或链路中断时，前端才调用有界的 Android PNG 单帧接口作为低频降级。iOS 仍使用已配对 USB/网络连接的 screenshotr PNG 采样。

Android 内嵌视频不启用控制 socket；鼠标键盘操作仍由用户显式打开的 scrcpy 交互窗口承担。iOS 的高帧率录屏/操控需要单独、可验证的 WDA/MJPEG 类适配器，不会用重复截图替代。

## Android 代理状态模型

UI 默认只创建 Reverse，不修改系统代理。只有用户明确选择“Reverse + 系统代理”或 LAN 的“设置系统代理”时，前端才调用代理写入命令。后端将以下五项作为一个不可拆分的快照：

- `http_proxy`
- `global_http_proxy_host`
- `global_http_proxy_port`
- `global_http_proxy_exclusion_list`
- `global_proxy_pac_url`

重复设置且当前快照仍是 Mobius 上次写入的值时，保留最初的恢复基线。恢复按钮和退出清理都会先对比整组快照；任一字段被外部工具改动后，Mobius 会留住外部新值并放弃本会话恢复。恢复“无有效代理”时先写入 `http_proxy=:0`，等待 Android 清理内存中的代理并广播 `PROXY_CHANGE`，再回写五项原始表示。由于 ADB 全局设置接口无法可靠地重建有效 PAC 或非空排除列表，在这类先前状态上的显式设置也会在任何修改前拒绝。

## 工具解析与受控分发

后端已接入持久化工具配置和统一解析器，优先级如下：

1. 用户显式选择的 `adb`、`scrcpy`、Frida CLI 路径，或用户指定的受控工具目录、iOS 工具目录。
2. 当前目标平台安装包的 `resources/tools` 目录。
3. `ANDROID_HOME`、`ANDROID_SDK_ROOT` 及常见 Android SDK 目录。
4. 系统 `PATH`。

解析结果会标记为 `configured`、`bundled`、`sdk` 或 `path`，设置页展示实际路径和来源。`get_tool_health` 将 `ffmpeg` 与 `scrcpy` 分开报告，也将 `idevicepair` 与 `idevicesyslog` 作为独立 iOS 工具检测；一个 libimobiledevice 程序就绪不代表其他程序必然存在。`ffmpeg` 可从受控工具目录或 `PATH` 解析，`scrcpy` 还必须能找到与客户端版本匹配的 Server 文件才能建立内嵌流。当前源码只提供 `resources/tools` 的目录约定，**没有附带第三方可执行文件，也不会自动下载**；这包括 `scrcpy-server` 和 `ffmpeg`。发行方只有在完成再分发许可证、NOTICE/SBOM、上游来源、SHA-256、依赖扫描和平台签名审查后，才能把工具放入随包目录；不得静默使用未经验证的文件。设备端 Frida Server 不走随包解析，始终由用户按版本和 ABI 选择。

## 测试策略

- 纯逻辑单元测试：解析 ADB 输出、端口映射、iOS 隧道方向/回环参数、libimobiledevice 固定调用、文件列表、地址与路径校验。
- 伪工具集成测试：使用可控的假 `adb` / `scrcpy` / `ffmpeg` / `idevice_*` / `iproxy` / `ssh` / `scp` 可执行程序覆盖超时、乱码、超大输出、链路中断、会话清理与失败码。
- 三平台构建测试：每次提交执行前端构建、`cargo check` 和 `cargo test`。
- 实体设备测试：Android USB/Wi-Fi/模拟器、内嵌连续视频与单帧降级、旋转布局、会话清理、截图/录屏/剪贴板；越狱 iOS 的已配对 screenshotr、四个 libimobiledevice 固定操作、USB iproxy、SSH `-L` / `-R` 与会话/退出清理。未被 `idevice_id -l/-n` 实际枚举的开发环境不能记为 iOS 真机通过。
- 发布验收：安装、升级、卸载、签名校验、SmartScreen/Gatekeeper、Linux udev 和 Wayland/X11。
