# 开发环境指南

## 通用版本

- Node.js：22 LTS，使用 `.nvmrc` 对齐。
- Rust：使用 `rust-toolchain.toml` 中的 stable 工具链。
- 包管理：pnpm 10.12.1，CI 使用 `pnpm-lock.yaml` 做冻结安装。
- Git：建议使用支持长路径和符号链接的较新版本。

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

## macOS

桌面开发只需 Xcode Command Line Tools：

```bash
xcode-select --install
```

安装常用外部工具的示例：

```bash
brew install android-platform-tools scrcpy ffmpeg libimobiledevice ideviceinstaller
```

调试页的 iOS 主机工具按钮需要 `ideviceinfo`、`idevicepair`、`ideviceinstaller` 和 `idevicesyslog`；屏幕预览/截图需要 `idevicescreenshot`。越狱 iOS 的 USB 隧道与 SSH 文件管理还要求 `iproxy`、`ssh` 和 `scp` 可用。macOS 通常自带 OpenSSH；如果现有 libimobiledevice 安装没有提供 `iproxy`，请按所用包管理器的拆分方式安装 libusbmuxd/usbmuxd 客户端工具。

Rust 建议通过 rustup 安装。若需要处理非越狱 iOS 调试、Developer Disk Image 或 iOS 目标本身，必须安装并至少启动一次完整 Xcode；仅有 Command Line Tools 不足以完成这些任务。

发布 DMG 还需要 Apple Developer Program 中有效的 Developer ID Application 身份和公证凭据。

## Windows

安装以下系统组件：

1. Microsoft C++ Build Tools，并选择“Desktop development with C++”。
2. Rust MSVC 工具链。
3. Node.js 22 LTS。
4. Microsoft Edge WebView2 Runtime。Windows 10/11 通常已包含，但 CI 或精简系统仍应检查。
5. Android SDK Platform Tools、完整的 Windows 版 scrcpy（含与客户端版本匹配的 `scrcpy-server`）和 FFmpeg。
6. iOS 设备发现、固定诊断与截图所需的 `idevice_id`、`ideviceinfo`、`idevicepair`、`ideviceinstaller`、`idevicescreenshot`、`idevicesyslog`，USB 隧道所需的 `iproxy`，以及 Windows OpenSSH Client 中的 `ssh`、`scp`。

Android 真机可能需要设备厂商的 ADB USB 驱动。iOS USB 功能还需要 Apple Mobile Device Support；仅复制 `idevice_*` 可执行文件通常不足以建立设备连接。

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
  patchelf \
  xdg-utils
```

移动设备工具的包名取决于发行版，Debian/Ubuntu 通常可从 `adb`、完整的 `scrcpy`、`ffmpeg`、`libimobiledevice-utils`、`usbmuxd`、提供 `iproxy` 的 libusbmuxd 工具包和 `openssh-client` 开始。还需配置允许当前用户访问 Android/iOS USB 设备的 udev 规则。

AppImage 并不消除内核、FUSE、图形栈、WebKitGTK 或设备规则差异。发布测试至少要覆盖目标发行版的 X11 与 Wayland 会话。

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

## 外部工具诊断

```bash
adb version
adb devices -l
scrcpy --version
ffmpeg -version
idevice_id --version
idevice_id -l
idevice_id -n
ideviceinfo --version
idevicepair --version
ideviceinstaller --version
idevicescreenshot --version
idevicesyslog --version
frida --version
aapt2 version
apkanalyzer --version
iproxy --help
ssh -V
```

`scp` 没有统一的跨平台版本参数，请直接在 `设置 → 工具链` 查看它的可用性和实际解析路径。该页也会独立报告 `idevicepair` 与 `idevicesyslog`，不会因其他 libimobiledevice 程序存在就假定它们可用。

注意：运行 `adb` 可能启动本机 ADB Server；实际启动内嵌屏幕会把与当前 scrcpy 版本匹配的 Server jar 临时推送到精确的 Android 设备，停止时尽力删除。`idevicescreenshot` 需要已信任设备与匹配的 Developer Disk Image；仅能 SSH 登录不代表 screenshotr 可用。CI 的普通单元测试不应依赖真实设备或直接执行这些诊断命令。

## 常见问题

### 浏览器里只有示例设备

这是预期行为。`pnpm run dev` 是纯 Web 预览；必须使用 `pnpm run tauri dev` 才能调用 Rust 和真实工具。

### 桌面应用找不到命令

先打开 `设置 → 工具链` 查看每项工具的解析来源和实际路径。查找顺序是：用户指定的单个工具或受控/iOS 工具目录、安装包 `resources/tools`、Android SDK 常见目录、系统 `PATH`。通过图形界面启动的应用可能继承与终端不同的环境变量，因此可以在设置页选择绝对路径并保存；保存时会验证路径和可执行权限。

当前源码只提供 `resources/tools` 的目录结构，**不附带第三方二进制，也不会自动下载**。请不要因为内嵌屏幕新增依赖而把 `scrcpy-server` 或 `ffmpeg` 直接提交到源码。发行方若要制作带工具的内部安装包，必须先完成再分发许可证、NOTICE/SBOM、上游来源、SHA-256 和平台签名审核。

### Android 设备页没有连续画面

Android 默认预览需要三项同时可用：`adb`、完整的 scrcpy 安装和 `ffmpeg`。请先在 `设置 → 工具链` 确认 `scrcpy` 与 `ffmpeg` 都是“就绪”，再核对 scrcpy 可执行文件附近是否有同一发行版的 `scrcpy-server`/`scrcpy-server.jar`。标准安装位置也可是相邻的 `share/scrcpy` 或 `lib/scrcpy` 目录；开发环境可用 `SCRCPY_SERVER_PATH` 指向经过审查且版本匹配的 Server 文件。

缺少任一依赖或视频链路中断时，界面会明确显示“已降级为画面采样”，并使用低频单帧 PNG；这不代表独立 scrcpy 交互窗口已启动。点击“重连”会重建受管视频会话，切换设备、暂停、离开页面或退出应用时会清理该会话。

### Android 显示 unauthorized

解锁设备并接受 RSA 调试授权；Mobius 会等待设备完成正常确认。

### iOS 列表为空

确认设备已解锁并信任当前电脑，再检查 libimobiledevice、usbmuxd 或 Apple Mobile Device 驱动。部分新 iOS 版本可能要求更新这些组件。`idevice_id -l` 或 `idevice_id -n` 必须实际返回目标 UDID 才能算设备通道可用；空输出只表示未发现设备，不能当作真机功能验收。

### iOS 主机工具按钮报错

- 先在 `设置 → 工具链` 确认对应的 `ideviceinfo`、`idevicepair`、`ideviceinstaller` 或 `idevicesyslog` 已就绪。四个按钮分别执行固定的设备信息、`validate`、`list --all` 和限时日志采样，不接受任意参数。
- 对 USB 设备使用本地 usbmuxd 通道；对 libimobiledevice 已配对的网络设备会附加 `-n`。SSH 手工端点不是这组主机工具的替代通道。
- `idevicesyslog` 默认采样 5 秒后由 Mobius 停止；因采样窗口到期而停止是正常结果，不是“实时日志已永久启动”。

### iOS 端口隧道无法创建

- USB `iproxy` 只能将本机 `127.0.0.1:主机端口` 转到指定 UDID 的设备端口，不支持设备→本机方向。
- SSH `-L` 用于本机→设备，`-R` 用于设备→本机；两者都必须复用已验证的 iOS SSH 会话，并且两端均只绑定 `127.0.0.1`。反向隧道的“本机端口”必须是已有本机服务的端口。
- 本机→设备方向会等待本地监听就绪；SSH `-R` 只能确认受管 SSH 子进程在启动窗口内未退出，不等于已对设备端发起业务连接。
- 关闭 SSH 会话会停止与它绑定的 `-L` / `-R` 隧道；独立 USB `iproxy` 需手动停止或等待应用退出清理。退出应用会停止所有还在登记表中的 iOS 隧道，不会扫描或终止其他工具启动的 `iproxy` / `ssh`。

### 越狱 iOS SSH 无法连接

- USB 模式先确认设备 UDID 可被 libimobiledevice 识别，`iproxy` 可启动，且设备上的 SSH 服务正在监听所填设备端口；主机端口可留空让 Mobius 自动选择。
- LAN 模式只接受私网、回环或链路本地的字面 IP，不接受主机名或公网目标。
- 默认密码模式使用 root / alpine；可在“修改连接设置”中更换账号或密码。密码只传给当次 OpenSSH 子进程的 askpass 环境，不会写入本地设置或日志。
- 私钥模式下，确认私钥是本机普通文件、权限足够严格，且对应公钥已加入设备账号的 `authorized_keys`。
- 允许目录不能是 `/`。目录必须存在，且登录账号需要拥有所选文件操作所需的权限。

### Frida 启动后立即退出

优先核对设备 ABI、Frida 客户端/Server 版本、可执行权限和 root/Gadget 模式。16.1.4、最新稳定版和自定义项只是本地配置槽；Server 文件由用户选择，Mobius 不会替换不匹配的二进制，也不会自动获取 root。自定义设备端口启动成功后会自动创建对应的本机 ADB forward。

### iOS 系统日志为空

先确认当前选择的日志来源：

- libimobiledevice 按钮使用主机 `idevicesyslog` 对精确 UDID 做限时采样，需要设备已配对/信任且工具链中该程序就绪。
- SSH 诊断先在 `调试 → iOS 工具 → 越狱 SSH` 的工具检测中查看 `Apple log` 是否可用。Mobius 优先读取最近 5 分钟的统一日志，不可用时回退到 `/var/log/syslog`；两者均不存在时会在结果区明确提示。

两条路径都是有界结果，不会在后台留下无限日志流。

### iOS IPA 无法通过 SSH 安装

- 先确认当前是 UID 0 的 Root SSH 会话。
- Mobius 只识别固定路径中的 `appinst` 或 `ipainstaller`；它不会替测试机安装这些工具。USB/usbmux 设备会优先使用主机上的 `ideviceinstaller`。
- 设备安装器的失败信息会原样返回；常见原因是签名、描述文件、信任或 AppSync 环境不兼容。
- `.app` 导出是分析归档，不能当作 IPA 直接安装。
