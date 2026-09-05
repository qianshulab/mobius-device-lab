import { AlertCircle, AppWindow, CheckCircle2, ChevronRight, FolderDown, Info, KeyRound, Laptop, Network, Palette, RefreshCw, Save, ShieldCheck, TerminalSquare, Wrench } from "lucide-react";
import { useState } from "react";
import type { AppSettings, ToolHealth } from "../types";
import { Button, Field, InlineNotice, Panel, StatusBadge, StatusDot } from "../components/Ui";
import { chooseDirectory, chooseLocalFile } from "../lib/dialog";

interface SettingsProps {
  settings: AppSettings;
  tools: ToolHealth[];
  group: SettingsGroup;
  onGroupChange: (group: SettingsGroup) => void;
  onSave: (settings: AppSettings) => void;
  onRefreshTools: () => void;
}

const groups = [
  { id: "toolchain", label: "工具链", icon: Wrench },
  { id: "network", label: "网络", icon: Network },
  { id: "storage", label: "文件与媒体", icon: FolderDown },
  { id: "security", label: "安全", icon: ShieldCheck },
  { id: "appearance", label: "外观", icon: Palette },
  { id: "about", label: "关于", icon: Info },
] as const;
export type SettingsGroup = typeof groups[number]["id"];
const sourceLabel: Record<NonNullable<ToolHealth["source"]>, string> = { configured: "已指定", bundled: "随包", sdk: "SDK", path: "PATH" };

function configurationInputFor(toolId: string) {
  if (toolId === "adb") return "tool-path-adb";
  if (toolId === "scrcpy") return "tool-path-scrcpy";
  if (toolId === "frida") return "tool-path-frida";
  if (["idevice_id", "ideviceinfo", "ideviceinstaller", "idevicescreenshot", "iproxy", "ssh", "scp"].includes(toolId)) return "tool-path-ios";
  return "tool-path-managed";
}

export default function SettingsPage({ settings, tools, group, onGroupChange, onSave, onRefreshTools }: SettingsProps) {
  const [draft, setDraft] = useState(settings);
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings);

  const set = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => setDraft((current) => ({ ...current, [key]: value }));
  const locateToolConfiguration = (toolId: string) => {
    onGroupChange("toolchain");
    window.requestAnimationFrame(() => window.requestAnimationFrame(() => {
      const input = document.getElementById(configurationInputFor(toolId));
      input?.scrollIntoView({ behavior: "smooth", block: "center" });
      input?.focus();
    }));
  };

  return (
    <div className="page settings-page">
      <div className="page-heading"><div><span className="eyebrow">WORKSPACE CONFIGURATION</span><h1>设置</h1><p>工具位置、网络范围、安全策略与应用信息。</p></div>{dirty && <Button variant="primary" icon={<Save size={15} />} onClick={() => onSave(draft)}>保存更改</Button>}</div>
      <div className="settings-layout">
        <nav className="settings-nav">{groups.map(({ id, label, icon: Icon }) => <button key={id} className={group === id ? "active" : ""} onClick={() => onGroupChange(id)}><Icon size={16} /><span>{label}</span><ChevronRight size={14} /></button>)}</nav>
        <main className="settings-content">
          {group === "toolchain" && <>
            <Panel title="工具链状态" action={<Button variant="ghost" icon={<RefreshCw size={14} />} onClick={onRefreshTools}>重新检测</Button>}>
              <div className="settings-tools">{tools.map((tool) => <div key={tool.id} title={tool.purpose}><StatusDot status={tool.state === "ready" ? "success" : tool.state === "warning" ? "warning" : "error"} /><div><strong>{tool.name}{tool.required && <em>核心</em>}</strong><span>{tool.path ?? tool.hint ?? tool.installHint ?? "未找到"}</span></div><code>{tool.source ? sourceLabel[tool.source] : tool.version || "—"}</code>{tool.state === "ready" ? <StatusBadge tone="success">就绪</StatusBadge> : <button type="button" className="text-button" onClick={() => locateToolConfiguration(tool.id)}>{tool.state === "warning" ? "检查" : "配置"}</button>}</div>)}</div>
            </Panel>
            <Panel title="自定义与受控工具目录">
              <div className="tool-path-grid">
                <Field label="ADB 可执行文件"><div className="path-input"><input id="tool-path-adb" value={draft.adbPath} onChange={(event) => set("adbPath", event.target.value)} placeholder="自动查找" /><button type="button" onClick={async () => { const selected = await chooseLocalFile("选择 adb 可执行文件"); if (selected) set("adbPath", selected); }}>选择</button></div></Field>
                <Field label="scrcpy 可执行文件"><div className="path-input"><input id="tool-path-scrcpy" value={draft.scrcpyPath} onChange={(event) => set("scrcpyPath", event.target.value)} placeholder="自动查找" /><button type="button" onClick={async () => { const selected = await chooseLocalFile("选择 scrcpy 可执行文件"); if (selected) set("scrcpyPath", selected); }}>选择</button></div></Field>
                <Field label="Frida CLI 可执行文件"><div className="path-input"><input id="tool-path-frida" value={draft.fridaPath} onChange={(event) => set("fridaPath", event.target.value)} placeholder="自动查找" /><button type="button" onClick={async () => { const selected = await chooseLocalFile("选择 frida 可执行文件"); if (selected) set("fridaPath", selected); }}>选择</button></div></Field>
                <Field label="iOS 工具目录"><div className="path-input"><input id="tool-path-ios" value={draft.iosToolsPath} onChange={(event) => set("iosToolsPath", event.target.value)} placeholder="idevice_* / iproxy / ssh / scp" /><button type="button" onClick={async () => { const selected = await chooseDirectory("选择 iOS 工具目录"); if (selected) set("iosToolsPath", selected); }}>选择</button></div></Field>
                <Field label="Mobius 受控工具目录" hint="适合组织统一分发经审核的多工具目录。"><div className="path-input"><input id="tool-path-managed" value={draft.managedToolsPath} onChange={(event) => set("managedToolsPath", event.target.value)} placeholder="可选" /><button type="button" onClick={async () => { const selected = await chooseDirectory("选择 Mobius 受控工具目录"); if (selected) set("managedToolsPath", selected); }}>选择</button></div></Field>
              </div>
              <div className="tool-path-footer"><span>保存时会验证绝对路径和可执行权限，不会运行所选文件。</span><Button variant="ghost" onClick={() => setDraft((current) => ({ ...current, adbPath: "", scrcpyPath: "", fridaPath: "", iosToolsPath: "", managedToolsPath: "" }))}>全部改为自动查找</Button></div>
            </Panel>
            <Panel title="工具查找策略">
              <div className="security-principles"><div><TerminalSquare /><span><strong>1 · 用户明确指定</strong><small>单个工具或组织维护的受控目录，优先级最高。</small></span></div><div><AppWindow /><span><strong>2 · 随安装包工具</strong><small>仅使用通过许可证、哈希和签名审查后放入 resources/tools 的版本。</small></span></div><div><KeyRound /><span><strong>3 · SDK 与系统</strong><small>再查找 Android SDK 常见目录，最后使用 PATH。</small></span></div><div><Laptop /><span><strong>Frida Server 例外</strong><small>设备端 Server 始终由用户为具体版本和 ABI 选择，不随包提供。</small></span></div></div>
              <p className="settings-note">当前源码包只带受控目录与解析器，不附带第三方二进制。发布版只有在完成许可证、NOTICE、SHA-256 清单和各平台签名审查后才应内置工具。</p>
            </Panel>
          </>}

          {group === "network" && <>
            <Panel title="局域网发现">
              <div className="settings-form"><Field label="高级扫描网段" hint="留空时自动识别电脑当前真实私网；仅接受 RFC1918 /24。"><input value={draft.scanCidr} onChange={(e) => set("scanCidr", e.target.value)} placeholder="自动识别" /></Field><Field label="ADB 扫描端口"><input value={draft.scanPort} onChange={(e) => set("scanPort", e.target.value.replace(/[^\d,]/g, ""))} /></Field></div>
            </Panel>
            <Panel title="测试代理默认值">
              <div className="settings-form"><Field label="默认代理主机" hint="127.0.0.1 表示优先使用 USB Reverse，无需查找电脑 IP。"><input value={draft.proxyHost} onChange={(e) => set("proxyHost", e.target.value)} placeholder="127.0.0.1" /></Field><Field label="默认监听端口"><input value={draft.proxyPort} onChange={(e) => set("proxyPort", e.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></Field></div>
            </Panel>
            <InlineNotice tone="info" title="无线调试优先级">已连接 USB → 自动发现当前私网 5555 → ADB mDNS / 配对码 → 高级手动地址。</InlineNotice>
          </>}

          {group === "storage" && <>
            <Panel title="电脑端保存目录">
              <div className="settings-form">
                <Field label="截图与录屏默认目录" hint="留空时每次保存都会询问；复制截图到剪贴板不需要目录。"><div className="path-input"><input value={draft.mediaDirectory} onChange={(event) => set("mediaDirectory", event.target.value)} placeholder="每次询问" /><button type="button" onClick={async () => { const selected = await chooseDirectory("选择截图与录屏保存目录"); if (selected) set("mediaDirectory", selected); }}>选择</button></div></Field>
                <Field label="应用包导出默认目录" hint="配置后，导出 APK 或 iOS .app 分析归档时不再重复选择目录。"><div className="path-input"><input value={draft.appExportDirectory} onChange={(event) => set("appExportDirectory", event.target.value)} placeholder="每次询问" /><button type="button" onClick={async () => { const selected = await chooseDirectory("选择应用包导出目录"); if (selected) set("appExportDirectory", selected); }}>选择</button></div></Field>
              </div>
              {(draft.mediaDirectory || draft.appExportDirectory) && <div className="button-row"><Button variant="ghost" onClick={() => setDraft((current) => ({ ...current, mediaDirectory: "", appExportDirectory: "" }))}>全部改为每次询问</Button></div>}
            </Panel>
            <InlineNotice tone="info" title="设备端临时文件会自动清理">截图和录屏由 Android 系统生成后直接取回电脑；成功或失败都会尽力删除 Mobius 创建的设备临时文件。</InlineNotice>
          </>}

          {group === "security" && <>
            <Panel title="安全策略">
              <div className="toggle-list">
                <label className="locked-setting"><div><strong>写入操作二次确认（固定启用）</strong><span>删除、写入型 Shell、重启等操作每次显示目标和影响，不能关闭。</span></div><input type="checkbox" checked readOnly disabled /><i /></label>
                <label><div><strong>日志自动脱敏</strong><span>导出时隐藏用户目录、设备标识符、私有 IP 与一次性配对信息。</span></div><input type="checkbox" checked={draft.redactLogs} onChange={(e) => set("redactLogs", e.target.checked)} /><i /></label>
              </div>
            </Panel>
            <Panel title="执行边界"><div className="security-principles"><div><CheckCircle2 /><span><strong>无本机 Shell 拼接</strong><small>调用外部工具时始终使用程序和参数数组。</small></span></div><div><CheckCircle2 /><span><strong>默认最小权限</strong><small>应用不会以管理员或 root 身份启动。</small></span></div><div><CheckCircle2 /><span><strong>会话资源归属</strong><small>只停止 Mobius 自己启动和跟踪的进程或隧道。</small></span></div><div><AlertCircle /><span><strong>自有设备范围</strong><small>只将自己管理的设备、应用和私有网段加入工作台。</small></span></div></div></Panel>
          </>}

          {group === "appearance" && <Panel title="界面">
            <div className="appearance-preview"><div className="theme-swatch active"><span className="dark-preview"><i /><i /><i /></span><strong>Graphite</strong><small>Burp 灵感暗色主题</small></div></div>
            <div className="toggle-list"><label><div><strong>紧凑模式</strong><span>缩小表格行高与页面间距，适合小屏幕。</span></div><input type="checkbox" checked={draft.compactMode} onChange={(e) => set("compactMode", e.target.checked)} /><i /></label></div>
          </Panel>}

          {group === "about" && <Panel className="about-panel">
            <div className="about-hero"><img src="/brand/mobius-mark.png" alt="Mobius" /><div><h2>Mobius</h2><p>Mobile Device Workbench</p><StatusBadge tone="warning">PREVIEW 0.1.0</StatusBadge></div></div>
            <p className="about-copy">为自有 Android 与越狱 iOS 测试设备打造的本地开发调试与管理工作台。一个代码库，面向 Windows、Linux 与 macOS 分别构建原生安装包。</p>
            <div className="about-meta"><span>界面：React + TypeScript</span><span>本机核心：Rust + Tauri 2</span><span>默认无遥测</span></div>
          </Panel>}
        </main>
      </div>
    </div>
  );
}
