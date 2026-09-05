# 开发环境指南

## 通用版本

- Node.js：22 LTS，使用 `.nvmrc` 对齐。
- Rust：使用 `rust-toolchain.toml` 中的 stable 工具链。
- 包管理：pnpm 10.12.1，CI 使用 `pnpm-lock.yaml` 做冻结安装。
- Git：建议使用支持长路径和符号链接的较新版本。
- 生成随包工具时还需要 Go 1.26.5、C 工具链、Make 与 NASM；普通安装包用户不需要这些构建依赖。

首次安装：

```bash
pnpm install --frozen-lockfile
```

浏览器预览使用内置模拟数据，不会执行真实设备命令：

```bash
pnpm run dev
```

连接真实本机工具时运行 Tauri：

```bash
pnpm run tauri dev
```

发行安装包已经带有目标平台的 ADB、scrcpy/Server、最小 FFmpeg、AAPT2、Detect It Easy 3.21 CLI/规则库、go-ios 与 Mobius SSH/SFTP。源码仓库不提交这些生成后二进制；第一次从源码连接真实设备或执行完整 APK 分析前，应按“生成受控工具资源”一节为当前平台准备一次资源目录。纯浏览器预览和不触及设备的单元测试不需要它。

## macOS

桌面开发至少需要 Xcode Command Line Tools；构建随包 FFmpeg 还需要 NASM：

```bash
xcode-select --install
brew install nasm
```

ADB、scrcpy/Server、FFmpeg、AAPT2、Detect It Easy CLI/规则库、go-ios 和 Mobius SSH/SFTP 都由构建脚本准备；libimobiledevice/ideviceinstaller/libusbmuxd 只在验证兼容回退时需要，可选安装，不是发行包首次使用要求。官方 Detect It Easy 3.21 Apple Silicon 包的最低系统是 macOS 13.0；应用本体在 macOS 12 上仍可运行，但该环境的 APK 加固扫描可能明确降级为“无法确定”。

Rust 建议通过 rustup 安装。若需要处理非越狱 iOS 调试、Developer Disk Image 或 iOS 目标本身，必须安装并至少启动一次完整 Xcode；仅有 Command Line Tools 不足以完成这些任务。

发布 DMG 还需要 Apple Developer Program 中有效的 Developer ID Application 身份和公证凭据。

## Windows

安装以下系统组件：

1. Microsoft C++ Build Tools，并选择“Desktop development with C++”。
2. Rust MSVC 工具链。
3. Node.js 22 LTS。
4. Microsoft Edge WebView2 Runtime。Windows 10/11 通常已包含，但 CI 或精简系统仍应检查。
5. 生成随包 FFmpeg 所需的 MSYS2 MINGW64 环境、GCC、Make、NASM、Python、tar 与 xz；发布工作流会自动配置这些组件。

ADB、scrcpy/Server、FFmpeg、AAPT2、Detect It Easy CLI/规则库、go-ios 和 Mobius SSH/SFTP 都由资源构建脚本准备，无需安装 Android SDK、独立 APK 加固识别工具、系统 OpenSSH 或 libimobiledevice。Android 真机仍可能需要设备厂商的 ADB USB 驱动；iOS USB 功能仍需要 Apple Mobile Device Support，单独复制任何主机 CLI 都不能替代驱动和服务。

发布工作流默认生成 NSIS 安装程序，因此不依赖 MSI 所需的 VBSCRIPT 可选功能。若日后启用 MSI，需要在构建主机上额外验证 WiX 与 VBSCRIPT。

## Linux（Debian/Ubuntu）

Tauri 2 的典型构建依赖：

```bash
sudo apt-get update
sudo apt-get install -y \
  libwebkit2gtk-4.1-dev \
  build-essential \
  curl \
  wget \
  file \
  libxdo-dev \
  libssl-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  nasm \
  patchelf \
  xdg-utils
```

越狱 SSH 会话不需要系统 `openssh-client`。iOS USB 通道还需要正在运行的 `usbmuxd` 与允许当前用户访问设备的 udev 规则；Android USB 也需要对应 udev 权限。`libimobiledevice-utils` 与提供 `iproxy` 的 libusbmuxd 工具包只用于兼容回退测试，ADB、scrcpy、FFmpeg、AAPT2、Detect It Easy 和桌面侧 SSH/SFTP 都不再是发行安装包的系统依赖。

AppImage 并不消除内核、FUSE、图形栈、WebKitGTK 或设备规则差异。发布测试至少要覆盖目标发行版的 X11 与 Wayland 会话。

## 生成受控工具资源

[`packaging/toolchain.lock.json`](../packaging/toolchain.lock.json) 固定每个上游归档的 URL、版本、字节数和 SHA-256，也固定 scrcpy 便携依赖、Android Platform Tools、FFmpeg、Detect It Easy CLI/规则库及所需 Qt 源码与许可材料、go-ios、Mobius SSH 模块与 Go 的源码/构建配置。选择当前平台目标后运行：

```bash
python3 scripts/prepare_tool_bundle.py \
  --target macos-aarch64 \
  --output src-tauri/resources/tools/macos-aarch64

python3 scripts/verify_tool_bundle.py \
  --target macos-aarch64 \
  --root src-tauri/resources/tools
```

可用目标为 `windows-x86_64`、`linux-x86_64`、`macos-aarch64` 与 `macos-x86_64`。Windows 应在 MSYS2 MINGW64 shell 中使用 `python`；其他系统使用 `python3`。将示例中的目标与输出目录同步替换，脚本会拒绝不匹配或过宽的输出路径。

准备过程会执行以下固定步骤：

1. 按锁文件验证 HTTPS 下载的精确大小与 SHA-256，并使用带条目/容量上限的安全解包。
2. 从 scrcpy 4.1 官方便携包提取客户端、匹配 Server、ADB 37.0.0 及 Windows 运行库；另下载精确 Google Platform Tools 37.0.0，逐字节核对 ADB/相邻 DLL 并收录对应 `NOTICE`。
3. 从锁定源码提取 scrcpy 便携依赖的许可和内嵌第三方声明；从 Google Maven AAPT2 产物提取二进制与 `NOTICE`。
4. 从 Detect It Easy 3.21 的目标官方包只准备控制台 `diec`、APK/DEX 规则库与相邻必要运行库。Linux 额外从哈希锁定的 Ubuntu 20.04 包中只提取 ICU 66、zlib、PCRE/PCRE2、double-conversion 和 GLib 的必要 SONAME 文件；同时从精确源码归档收集 Detect It Easy、内嵌依赖、动态链接 Qt 模块以及这些 Linux 库的许可/重链材料和 Ubuntu 打包补丁。
5. 从锁定源码构建最小 LGPL FFmpeg 9.0.1；Windows 应用公开的原生计时补丁，锁定编译器版本并审计 PE 导入，以拒绝 `libwinpthread` 等未随包运行库；macOS 应用主体默认固定 12.0 构建下限，但不抬高调用者已设的更低值。同时使用 Go 1.26.5 对 go-ios 1.3.2 应用公开的 loopback/version 补丁后构建 `1.3.2-mobius.1`。
6. 收集许可证、Qt 动态链接与重链说明、Go 运行时/标准库许可、模块清单与依赖许可证，为所有随包文件生成大小、权限和 SHA-256 `manifest.json`。
7. 在 `windows-x86_64`、`linux-x86_64`、`macos-aarch64` 与 `macos-x86_64` 四个目标上验证文件集、目标架构、Detect It Easy 规则库/运行库/许可材料，并执行原生版本与扫描冒烟测试。Linux 验证还会用纯 Python 解析每个 ELF 的 `DT_NEEDED`/`DT_SONAME`，锁定除 glibc、C++ 运行时和动态加载器外的完整闭包，漏包或多包都会失败。macOS 发布任务之后会签名 Mach-O 文件、刷新清单并再次验证。

工具只在构建/开发准备阶段联网取得，已安装应用不会运行下载器或静默替换它们。生成目录由 `.gitignore` 排除；版本、补丁、脚本、锁文件与通用第三方声明进入源码审查。

## 验证命令

```bash
pnpm run check
pnpm run build
cargo fmt --all --manifest-path src-tauri/Cargo.toml -- --check
cargo check --locked --all-targets --manifest-path src-tauri/Cargo.toml
cargo clippy --locked --all-targets --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --locked --all-targets --manifest-path src-tauri/Cargo.toml
```

生成当前平台安装包：

```bash
pnpm run build
pnpm run tauri build
```

产物通常位于 `src-tauri/target/release/bundle/`。不同操作系统必须在对应的原生环境中构建并测试；日常开发机不需要安装另外两个系统的完整交叉编译链。

## 工具诊断

```bash
src-tauri/resources/tools/macos-aarch64/adb version
src-tauri/resources/tools/macos-aarch64/scrcpy --version
src-tauri/resources/tools/macos-aarch64/ffmpeg -version
src-tauri/resources/tools/macos-aarch64/aapt2 version
src-tauri/resources/tools/macos-aarch64/die/diec -v
src-tauri/resources/tools/macos-aarch64/ios version
src-tauri/resources/tools/macos-aarch64/ios list --details
src-tauri/resources/tools/macos-aarch64/ssh -V
src-tauri/resources/tools/macos-aarch64/scp -V
```

上例为 Apple Silicon 开发目录；其他平台替换目标目录和 `.exe` 后缀即可。普通用户直接在 `设置 → 工具链` 查看随包工具版本、来源和解析路径更可靠。主机 Frida CLI 不属于运行依赖或健康检查项。

注意：运行 `adb` 可能启动本机 ADB Server；实际启动内嵌屏幕会把与当前 scrcpy 版本匹配的 Server jar 临时推送到精确的 Android 设备，停止时尽力删除。go-ios 截图仍需要已信任设备与匹配的 Developer Disk Image；仅能 SSH 登录不代表 screenshotr 可用。CI 的普通单元测试不应依赖真实设备或直接执行这些诊断命令。

## 常见问题

### 浏览器里只有示例设备

这是预期行为。`pnpm run dev` 是纯 Web 预览；必须使用 `pnpm run tauri dev` 才能调用 Rust 和真实工具。

### 桌面应用找不到命令

正式安装包应直接显示 ADB、scrcpy、FFmpeg、AAPT2、Detect It Easy、go-ios、SSH 与 SCP 为“随包 / 就绪”。Detect It Easy 还要求 `diec` 相邻的 `db` 规则库和 Linux 的完整 ELF 运行库闭包存在；若缺失，请先重新安装同一官方 Release，并核对杀毒软件或企业终端策略是否隔离了资源文件。源码开发则先执行上文的准备与验证脚本。

工具查找顺序是：有效的用户指定单个工具或受控/iOS 工具目录、安装包 `resources/tools`、Android SDK 常见目录、系统 `PATH`。升级遗留的显式路径若已失效，解析器可安全回到受控随包副本；不会在这种情况下悄悄改用未知的 SDK/PATH 版本。设置页保存路径时会验证绝对路径和可执行权限。

### Android 设备页没有连续画面

Android 默认预览需要随包 `adb`、scrcpy 4.1 客户端/Server 和最小 FFmpeg 同时可用。先在 `设置 → 工具链` 确认三项均为“随包 / 就绪”；若不是，重新安装或重新生成当前目标的资源。只有开发者显式覆盖 scrcpy 时，才需要自行保证客户端与相邻 `scrcpy-server`/`scrcpy-server.jar` 版本一致；也可用 `SCRCPY_SERVER_PATH` 指向经过审查且匹配的 Server 文件。

缺少任一依赖或视频链路中断时，界面会明确显示“已降级为画面采样”，并使用低频单帧 PNG；这不代表独立 scrcpy 交互窗口已启动。点击“重连”会重建受管视频会话，切换设备、暂停、离开页面或退出应用时会清理该会话。

上述清理只针对投屏流。用户已启动的 Android 录屏会跨设备切换和页面导航继续运行；回到工作台后显式点击“停止录屏”才会完成 MP4 封装、拉取和保存。若用户直接退出应用，退出清理会对所有仍受管录屏尽力执行同样的收尾。

### APK 加固特征结果为什么是“无法确定”

这是三态设计的安全结果，不等于“未发现已知特征”。Mobius 只使用随包 Detect It Easy 3.21 和相邻规则库，扫描最多运行 25 秒；CLI/数据库缺失、超时、输出截断、异常退出或无效 JSON 都会返回“无法确定”。只有一次完整扫描零命中时才显示“未发现已知特征”；两者都不能证明 APK 绝对安全或未加固。

### Android 显示 unauthorized

解锁设备并接受 RSA 调试授权；Mobius 会等待设备完成正常确认。

### iOS 列表为空

确认设备已解锁并信任当前电脑，再在工具链中确认随包 go-ios 就绪。Windows 检查 Apple Mobile Device 驱动/服务，Linux 检查 usbmuxd 与 udev 权限。部分新 iOS 版本还可能要求更新系统集成或挂载对应 Developer Disk Image。`ios list --details` 必须实际返回目标 UDID 才能算设备通道可用；若启用兼容回退，`idevice_id -l/-n` 的结果也可用于定位问题。空输出只表示未发现设备，不能当作真机功能验收。

### iOS 主机工具按钮报错

- 先在 `设置 → 工具链` 确认 go-ios 为“随包 / 就绪”。四个按钮分别执行固定的设备信息、只读配对状态验证、应用清单和限时日志采样，不接受任意程序名或参数。
- 对 USB/已配对网络设备使用 go-ios；单项失败时，已配置的 libimobiledevice 程序可自动兼容回退。SSH 手工端点不是这组主机工具的替代通道。
- 系统日志默认采样 5 秒后由 Mobius 停止；因采样窗口到期而停止是正常结果，不是“实时日志已永久启动”。

### iOS 端口隧道无法创建

- USB 模式优先使用精确版本的随包 go-ios，将本机 `127.0.0.1:主机端口` 转到指定 UDID 的设备端口；只有 `1.3.2-mobius.1` loopback 加固构建可走这条路径。失败时才尝试已配置的 `iproxy`，两者都不支持设备→本机方向。
- SSH `-L` 用于本机→设备，`-R` 用于设备→本机；两者都必须复用已验证的 iOS SSH 会话，并且两端均只绑定 `127.0.0.1`。反向隧道的“本机端口”必须是已有本机服务的端口。
- 本机→设备方向会等待本地监听就绪；SSH `-R` 只能确认受管 SSH 子进程在启动窗口内未退出，不等于已对设备端发起业务连接。
- 关闭 SSH 会话会停止与它绑定的 `-L` / `-R` 隧道；独立 USB 转发需手动停止或等待应用退出清理。退出应用会停止所有还在登记表中的 iOS 隧道，不会扫描或终止其他工具启动的 go-ios、`iproxy` 或 `ssh`。

### 越狱 iOS SSH 无法连接

- USB 模式先确认设备 UDID 可被 go-ios 识别，且设备上的 SSH 服务正在监听所填设备端口；主机端口可留空让 Mobius 自动选择。若转发回退到 `iproxy`，再核对该备用工具配置。
- LAN 模式只接受私网、回环或链路本地的字面 IP，不接受主机名或公网目标。
- 默认密码模式使用 root / alpine；可在“修改连接设置”中更换账号或密码。密码由内存中的一次性回环 broker 交给随包客户端，不会进入进程参数或持久环境，也不会写入本地设置或日志。
- 私钥模式下，确认私钥是本机普通文件、权限足够严格，且对应公钥已加入设备账号的 `authorized_keys`。
- 允许目录不能是 `/`。目录必须存在，且登录账号需要拥有所选文件操作所需的权限。

### Frida 启动后立即退出

优先核对设备 ABI、设备端 Server 版本、可执行权限和 root/Gadget 模式。Mobius 没有主机 Frida CLI 依赖；16.1.4、最新稳定版和自定义项只是本地配置槽，Server 文件由用户选择。Mobius 不会替换不匹配的二进制，也不会自动获取 root。自定义设备端口启动成功后会自动创建对应的本机 ADB forward。

### iOS 系统日志为空

先确认当前选择的日志来源：

- 主机工具按钮优先使用随包 go-ios 对精确 UDID 做限时采样，需要设备已配对/信任；已配置的 `idevicesyslog` 只作为兼容回退。
- SSH 诊断先在 `调试 → iOS 工具 → 越狱 SSH` 的工具检测中查看 `Apple log` 是否可用。Mobius 优先读取最近 5 分钟的统一日志，不可用时回退到 `/var/log/syslog`；两者均不存在时会在结果区明确提示。

两条路径都是有界结果，不会在后台留下无限日志流。

### iOS IPA 无法通过 SSH 安装

- 先确认当前是 UID 0 的 Root SSH 会话。
- Mobius 只识别固定路径中的 `appinst` 或 `ipainstaller`；它不会替测试机安装这些工具。USB/usbmux 设备优先使用随包 go-ios，已配置的 `ideviceinstaller` 只作为兼容回退。
- 设备安装器的失败信息会原样返回；常见原因是签名、描述文件、信任或 AppSync 环境不兼容。
- `.app` 导出是分析归档，不能当作 IPA 直接安装。
