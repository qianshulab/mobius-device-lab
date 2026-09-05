# 架构说明

## 目标

Mobius Device Lab 采用一套源码、多个原生发行包的结构。界面在三个桌面系统上保持一致，设备操作则由 Rust 核心使用当前操作系统上的受信任命令行工具完成。

```text
React / TypeScript UI
        │  typed Tauri invoke
        ▼
Rust 命令边界 ── 输入校验 ── 会话资源归属
        │
        ├── 包分析: ZIP / plist / Mach-O / 随包 AAPT2 / apkanalyzer 回退
        ├── Android: 随包 adb / scrcpy + scrcpy-server / 最小 FFmpeg / 用户提供的 frida-server
        └── iOS: 随包 go-ios / 随包 Mobius SSH/SFTP / libimobiledevice 回退
```

## 目录职责

| 路径 | 职责 |
| --- | --- |
| `src/` | 页面、组件、前端类型、Tauri 调用适配与浏览器预览数据 |
| `src-tauri/src/commands/` | 面向界面的窄命令接口；每项操作都有明确输入结构 |
| `src-tauri/src/commands/ios_native.rs` | 把 go-ios 的设备发现、信息、安装、截图与固定诊断收敛为结构化内部适配器 |
| `src-tauri/src/commands/ios_host_tools.rs` | 将 go-ios 信息/应用/日志及可选 libimobiledevice 回退约束为四个固定主机诊断 |
| `src-tauri/src/commands/ios_ports.rs` | 创建、列出和停止 USB go-ios/`iproxy` 与 SSH `-L` / `-R` 回环隧道，并管理其子进程归属 |
| `src-tauri/src/validation.rs` | 设备标识、地址、端口、远端路径和本地路径校验 |
| `src-tauri/src/runner.rs` | 外部进程启动、超时、输出截断、错误归一化和敏感值替换 |
| `src-tauri/src/state.rs` | 当前会话创建的内嵌屏幕流与受管录屏、代理整组快照、Android 映射、Frida 进程、iOS SSH 会话与受管 iOS 隧道子进程归属信息 |
| `src-tauri/src/toolchain.rs` | 工具链配置持久化、来源优先级、可执行文件校验与跨平台解析 |
| `packaging/toolchain.lock.json` | 锁定随包工具版本、上游 HTTPS 来源、精确大小、SHA-256 与源码构建参数 |
| `packaging/patches/`、`scripts/` | 保存 go-ios 的公开加固补丁，以及安全下载、构建、逐文件清单验证和第三方源码归档脚本 |
| `src-tauri/resources/tools/` | 构建时生成的目标平台工具、运行库、`manifest.json`、许可证、NOTICE、补丁和构建记录 |
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

- 设备与应用：随包 go-ios 使用 `list --details` 合并枚举 USB 与已配对网络设备（USB 优先去重），使用 `info` 读取属性；离线解析 IPA；USB 默认使用 `install`，Root SSH 会话可探测测试机已有的 `appinst` / `ipainstaller`。单项失败时，可回退到用户已有的 `idevice_id`、`ideviceinfo` 或 `ideviceinstaller`。
- 主机工具：调试页只向后端发送 `deviceInfo / pairing / apps / syslog` 枚举。后端优先构造绑定精确 UDID 的 go-ios `info`、`apps --all` 和 `syslog`；配对状态采用只读信息连接验证，不触发重新配对。日志以默认 5 秒窗口采样，所有输出均清理控制字符并限长。已配置的 libimobiledevice 工具是兼容回退，而不是默认依赖。
- SSH 应用工作流：从固定 iOS 应用目录读取有界 `Info.plist` 元数据；安装临时文件位于会话首个允许根的 `.mobius-runtime`；导出仅生成开发分析 `.app` 归档，不重建 IPA。
- SSH 文件：USB 模式由受管本机回环转发将设备 SSH 端口映射到 `127.0.0.1`；LAN 模式只接受私网、回环或链路本地字面 IP。随包客户端直接从一次性回环 broker 取得密码，私钥模式只读取用户明确选择的未加密密钥；两者都不经过主机 Shell。
- 端口隧道：USB 首选随包、精确版本校验通过的 go-ios `1.3.2-mobius.1`，其公开补丁把监听收紧到 `127.0.0.1`；启动或就绪检测失败时才尝试可选 `iproxy` 回退。两者只实现本机→设备，设备服务由 UDID 与 usbmuxd 通道选择，不使用设备 IP。已验证 SSH 会话上的 `-L` 实现本机→设备，`-R` 实现设备→本机；SSH 客户端请求的监听端和目标端均为 `127.0.0.1`，并固定包含 `ExitOnForwardFailure=yes`。`-R` 的最终监听受设备 sshd 策略约束；启用 `GatewayPorts yes` 可能把请求的回环绑定改写为通配监听，因此此配置下不创建反向隧道。未经 Mobius 补丁和版本校验的 go-ios 不用于转发。
- 隧道生命周期：后端保存不可猜测隧道 ID、UDID、可选 `sessionId`、方向、端口和直接子进程。列表时检测并移除已退出子进程；显式停止只处理精确 ID。关闭 SSH 会话清理与该 `sessionId` 绑定的 `-L` / `-R` 隧道；独立 USB 转发保留到显式停止或应用退出。退出会清理全部仍受管的 iOS 隧道。
- 会话建立时先验证密码或私钥登录并规范化允许目录；浏览、上传、下载、新建和删除都必须留在这些根目录内，拒绝通过符号链接越界，且不能删除允许根本身。
- 当前产品范围只考虑用户拥有或获准测试的越狱设备；设备现有的越狱、AppSync、签名与信任状态均保持不变。
- 屏幕：go-ios `screenshot` 只能面向精确 UDID 的已配对 USB/网络 screenshotr 服务，失败时可回退到已配置的 `idevicescreenshot`。后端每次重新核对目标、在私有临时目录采集、校验 PNG/尺寸/像素/大小，再内嵌显示或写入剪贴板/用户目录。`ios-ssh:*` 手工端点在调用外部屏幕工具前就会被拒绝。
- iOS Frida 进程由 Root SSH 会话精确归属，设备和主机两端均只绑定回环地址。系统概览、进程、固定路径工具检测和 syslog 使用后端白名单、枚举请求和有界输出；Respring/重启还要求后端短时单次票据与实际 SSH 目标复核。

## 信息架构

侧栏按用户对象而不是底层命令分类：

1. `工作台`：固定启动页，首屏展示 Android 内嵌 scrcpy 连续视频或 iOS 配对 screenshotr 采样画面；使用“紧凑纵向手机画面 + 屏幕操作 + 设备连接”布局。设备完整信息只在二级管理弹窗展示。
2. `应用`：本地 APK/IPA 分析、安装、设备应用与导出。
3. `文件`：远端文件浏览和传输。
4. `网络`：Android 测试代理与 ADB forward/reverse；iOS USB go-ios（`iproxy` 回退）本机转发与 SSH `-L` / `-R` 双向回环隧道。
5. `调试`：Frida、系统/进程、iOS go-ios 固定主机工具与高级 Shell。
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

后端已接入持久化工具配置和统一解析器。发行安装默认直接命中对应目标的受控随包工具，不需要用户配置 `PATH`；开发者仍可覆盖，完整优先级如下：

1. 用户显式选择的 `adb`、`scrcpy` 路径，或用户指定的受控工具目录、iOS/SSH 备用工具目录。
2. 当前目标平台安装包的 `resources/tools` 目录。
3. `ANDROID_HOME`、`ANDROID_SDK_ROOT` 及常见 Android SDK 目录。
4. 系统 `PATH`。

解析结果会标记为 `configured`、`bundled`、`sdk` 或 `path`，设置页展示实际路径和来源。失效的预览版显式路径可以回到已审查的随包副本，但不会进一步静默替换为未知 SDK/PATH 文件。健康检查的必需项为 `adb`、`scrcpy`、`ffmpeg`、`aapt2`、`ios`（go-ios）、`ssh` 与 `scp`；主机 Frida CLI、`apkanalyzer` 和 libimobiledevice 工具都不是发行包的核心依赖。

每个原生发布任务依据 `packaging/toolchain.lock.json` 生成 `resources/tools/<target>`：

- scrcpy 4.1 官方便携归档提供客户端、匹配 `scrcpy-server` 与 ADB 37.0.0；Windows 还保留同归档运行库。
- Google Maven AAPT2 `9.4.0-15978811` 提供 APK 分析器与完整 `NOTICE`。
- FFmpeg `9.0.1` 从固定源码构建，只启用 H.264→MJPEG 所需的 LGPL 组件，不启用 GPL、nonfree、网络或外部编解码器。
- go-ios `1.3.2-mobius.1` 从固定提交构建，使用仓库公开补丁标记版本，并将转发与可选截图服务绑定从所有网卡改为 `127.0.0.1`。
- Mobius SSH/SFTP `0.2.0` 从仓库内审查源码和锁定 Go 模块构建，只接受后端生成的参数子集，支持密码/私钥执行、单文件 SFTP 和回环 `-L`/`-R`。首次主机密钥写入应用私有 `known_hosts`，变更后拒绝连接。

准备脚本在解包前核对 HTTPS 来源、精确大小和 SHA-256，限制归档成员类型与展开容量；源码归档中的许可只按锁定路径提取普通文件，不跟随符号链接。验证脚本核对目标架构、逐文件清单、执行权限和原生版本冒烟测试。生成的 `manifest.json` 与工具同包，目标 ADB/AAPT2 NOTICE、scrcpy 便携依赖许可/内嵌声明、FFmpeg 配置、Go 许可和 go-ios/Mobius SSH 依赖许可位于相邻 `licenses/`。Release 另附 scrcpy 及其便携依赖、FFmpeg、go-ios/Go、Mobius SSH 模块的完整第三方源码、链接索引与构建/重链脚本归档，其哈希进入 `SHA256SUMS.txt`。工具不会在已安装应用运行时下载或静默更新。

越狱会话的 `ssh`/`scp` 已随安装包提供，不依赖系统 OpenSSH 或 `PATH`。Windows Apple Mobile Device 驱动/服务、Linux usbmuxd/udev、设备信任、Developer Disk Image、越狱 SSH 服务、AppSync 和设备端安装器属于系统/设备集成，不能打包为普通应用资源。设备端 Frida Server 也不走随包解析，始终由用户按版本和 ABI 选择；Mobius 没有主机 Frida CLI 依赖。

## 测试策略

- 纯逻辑单元测试：解析 ADB/go-ios 输出、端口映射、iOS 隧道方向/回环与精确加固版本、libimobiledevice 固定回退、文件列表、地址与路径校验。
- 伪工具集成测试：使用可控的假 `adb` / `scrcpy` / `ffmpeg` / `ios` / `idevice_*` / `iproxy` / `ssh` / `scp` 可执行程序覆盖超时、乱码、超大输出、链路中断、会话清理与失败码。
- 工具资源测试：原生 runner 重新下载锁定归档、比对 ADB、验证 scrcpy 依赖许可，从源码构建 FFmpeg/go-ios/Mobius SSH，运行 SSH 安全边界单测，验证逐文件哈希、目标架构与版本冒烟；macOS 签名后刷新并再次验证清单。
- 三平台构建测试：每次提交执行前端构建、`cargo check` 和 `cargo test`。
- 实体设备测试：Android USB/Wi-Fi/模拟器、内嵌连续视频与单帧降级、旋转布局、会话清理、截图/录屏/剪贴板；越狱 iOS 的已配对 screenshotr、四个 go-ios 固定操作与 libimobiledevice 回退、go-ios/`iproxy` USB 转发、SSH `-L` / `-R` 与会话/退出清理。未被 `ios list --details`（或明确测试的回退通道）实际枚举的开发环境不能记为 iOS 真机通过。
- 发布验收：安装、升级、卸载、签名校验、SmartScreen/Gatekeeper、Linux udev 和 Wayland/X11。
