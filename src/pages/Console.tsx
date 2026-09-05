import { Circle, Code2, Copy, Download, Eraser, Play, Search, ShieldAlert, Smartphone, TerminalSquare } from "lucide-react";
import { FormEvent, KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { writeClipboardText } from "../lib/clipboard";
import type { Device } from "../types";
import { Button, EmptyState, InlineNotice, Modal, Panel, StatusBadge, StatusDot } from "../components/Ui";

interface OutputEntry {
  id: string;
  command: string;
  output: string;
  error?: string;
  startedAt: string;
  duration?: number;
  running?: boolean;
}

interface ConsoleProps {
  activeDevice?: Device;
  initialCommand?: string;
  notify: (type: "success" | "error" | "info" | "warning", title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: "success" | "warning" | "error" | "running" | "info") => void;
}

interface ConsoleSessionCache {
  command: string;
  entries: OutputEntry[];
  history: string[];
  filter: string;
}

const consoleSessions = new Map<string, ConsoleSessionCache>();
const runningDeviceCommands = new Set<string>();

const suggestedCommands = [
  { label: "设备信息", command: "getprop ro.product.model" },
  { label: "系统版本", command: "getprop ro.build.version.release" },
  { label: "CPU 架构", command: "getprop ro.product.cpu.abi" },
  { label: "电池状态", command: "dumpsys battery" },
  { label: "前台窗口", command: "dumpsys window windows" },
  { label: "用户应用", command: "pm list packages -3" },
  { label: "系统代理", command: "settings get global http_proxy" },
  { label: "网络路由", command: "ip route" },
  { label: "磁盘空间", command: "df -h" },
];

const readOnlyCommand = /^(?:getprop(?:\s+[\w.\-]+)?|pm\s+list(?:\s+[^\s]+)*|ps(?:\s+[^\s]+)*|ip\s+(?:addr|route|link)(?:\s+[^\s]+)*|df(?:\s+[^\s]+)*|ls(?:\s+[^\s]+)*|id|whoami|uname(?:\s+[^\s]+)*|getenforce|settings\s+get\s+(?:global|secure|system)\s+[\w.\-]+|dumpsys\s+(?:battery|window(?:\s+windows)?|activity\s+activities|meminfo(?:\s+[\w.]+)?|package(?:\s+[\w.]+)?))$/;
const shellControlSyntax = /[;&|><`$()\n\r]/;

function outputText(entries: OutputEntry[], device: Device) {
  const header = `Mobius Device Console\nDevice: ${device.name} (${device.id})\nExported: ${new Date().toISOString()}\n`;
  return `${header}\n${entries.map((entry) => [
    `[${entry.startedAt}] $ ${entry.command}`,
    entry.output,
    entry.error ? `ERROR: ${entry.error}` : "",
    entry.duration === undefined ? "" : `[${entry.duration} ms]`,
  ].filter(Boolean).join("\n")).join("\n\n")}\n`;
}

export default function ConsolePage({ activeDevice, initialCommand = "", notify, record }: ConsoleProps) {
  const cachedSession = activeDevice ? consoleSessions.get(activeDevice.id) : undefined;
  const [command, setCommand] = useState(initialCommand || cachedSession?.command || "");
  const [entries, setEntries] = useState<OutputEntry[]>(cachedSession?.entries ?? []);
  const [history, setHistory] = useState<string[]>(cachedSession?.history ?? []);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const [running, setRunning] = useState(() => !!activeDevice && runningDeviceCommands.has(activeDevice.id));
  const [filter, setFilter] = useState(cachedSession?.filter ?? "");
  const [pendingCommand, setPendingCommand] = useState<string>();
  const endRef = useRef<HTMLDivElement>(null);
  const latestSession = useRef<ConsoleSessionCache>({ command, entries, history, filter });
  const mountedRef = useRef(false);
  const deviceId = activeDevice?.id ?? "";
  const activeDeviceIdRef = useRef(deviceId);
  activeDeviceIdRef.current = deviceId;

  useEffect(() => { if (initialCommand.trim()) setCommand(initialCommand); }, [initialCommand]);
  useEffect(() => { latestSession.current = { command, entries, history, filter }; }, [command, entries, history, filter]);
  useLayoutEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  useLayoutEffect(() => {
    if (!deviceId) {
      setCommand("");
      setEntries([]);
      setHistory([]);
      setFilter("");
      setRunning(false);
      setPendingCommand(undefined);
      return;
    }
    const next = consoleSessions.get(deviceId) ?? { command: initialCommand, entries: [], history: [], filter: "" };
    setCommand(initialCommand || next.command);
    setEntries(next.entries);
    setHistory(next.history);
    setFilter(next.filter);
    setRunning(runningDeviceCommands.has(deviceId));
    setPendingCommand(undefined);
    setHistoryIndex(-1);
    latestSession.current = next;
    return () => { consoleSessions.set(deviceId, latestSession.current); };
  }, [deviceId]);

  const updateTargetEntries = (targetDeviceId: string, update: (current: OutputEntry[]) => OutputEntry[]) => {
    const base = activeDeviceIdRef.current === targetDeviceId ? latestSession.current : consoleSessions.get(targetDeviceId);
    if (!base) return;
    const next = { ...base, entries: update(base.entries) };
    consoleSessions.set(targetDeviceId, next);
    if (mountedRef.current && activeDeviceIdRef.current === targetDeviceId) {
      latestSession.current = next;
      setEntries(next.entries);
    }
  };

  const copyAll = async () => {
    if (!activeDevice || !entries.length) return;
    try {
      await writeClipboardText(outputText(entries, activeDevice));
      notify("success", "控制台输出已复制", `共 ${entries.length} 条命令记录`);
    } catch (error) { notify("error", "复制失败", error instanceof Error ? error.message : String(error)); }
  };

  const exportOutput = () => {
    if (!activeDevice || !entries.length) return;
    const blob = new Blob([outputText(entries, activeDevice)], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = url;
    anchor.download = `mobius-shell-${new Date().toISOString().replace(/[:.]/g, "-")}.txt`;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
    notify("success", "控制台输出已导出", `${entries.length} 条命令记录`);
  };

  const runCommand = async (trimmed: string) => {
    if (!trimmed || !activeDevice || activeDevice.platform !== "android" || runningDeviceCommands.has(activeDevice.id)) return;
    const targetDevice = activeDevice;
    const targetDeviceId = targetDevice.id;
    const id = crypto.randomUUID();
    const started = performance.now();
    const nextEntries = [...entries, { id, command: trimmed, output: "", startedAt: new Date().toLocaleTimeString("zh-CN"), running: true }];
    const nextHistory = [trimmed, ...history.filter((item) => item !== trimmed)].slice(0, 100);
    const startedSession = { command: "", entries: nextEntries, history: nextHistory, filter };
    consoleSessions.set(targetDeviceId, startedSession);
    latestSession.current = startedSession;
    setEntries(nextEntries);
    setHistory(nextHistory);
    setHistoryIndex(-1);
    setCommand("");
    runningDeviceCommands.add(targetDeviceId);
    setRunning(true);
    record("开始执行 ADB Shell", `${targetDevice.name} · ${trimmed}`, "info");
    try {
      const result = await api.shell(targetDeviceId, trimmed);
      updateTargetEntries(targetDeviceId, (current) => current.map((entry) => entry.id === id ? { ...entry, output: result.stdout ?? result.message, error: result.success ? result.stderr : result.message, running: false, duration: Math.round(performance.now() - started) } : entry));
      if (!result.success) { notify("error", `${targetDevice.name} 命令失败`, result.message); record("ADB Shell 执行失败", `${targetDevice.name} · ${trimmed}`, "error"); }
      else record("ADB Shell 执行完成", `${targetDevice.name} · ${trimmed}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      updateTargetEntries(targetDeviceId, (current) => current.map((entry) => entry.id === id ? { ...entry, error: message, running: false, duration: Math.round(performance.now() - started) } : entry));
      notify("error", `${targetDevice.name} 命令失败`, message);
      record("ADB Shell 执行失败", `${targetDevice.name} · ${trimmed}`, "error");
    } finally {
      runningDeviceCommands.delete(targetDeviceId);
      if (mountedRef.current && activeDeviceIdRef.current === targetDeviceId) {
        setRunning(false);
        requestAnimationFrame(() => endRef.current?.scrollIntoView({ behavior: "smooth" }));
      }
    }
  };

  const execute = (event?: FormEvent) => {
    event?.preventDefault();
    const trimmed = command.trim();
    if (!trimmed) return;
    if (!readOnlyCommand.test(trimmed) || shellControlSyntax.test(trimmed)) setPendingCommand(trimmed);
    else void runCommand(trimmed);
  };

  const historyKey = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
    event.preventDefault();
    const next = event.key === "ArrowUp" ? Math.min(historyIndex + 1, history.length - 1) : Math.max(historyIndex - 1, -1);
    setHistoryIndex(next);
    setCommand(next < 0 ? "" : history[next]);
  };

  if (!activeDevice) return <div className="page console-page"><div className="page-heading"><div><span className="eyebrow">DEVICE-BOUND TERMINAL</span><h1>控制台</h1><p>高级设备命令与任务输出。</p></div></div><Panel><EmptyState icon={<TerminalSquare size={30} />} title="请先选择设备" detail="每个控制台会话只绑定一台明确的目标设备。" /></Panel></div>;

  if (activeDevice.state !== "online") return <div className="page console-page"><div className="page-heading"><div><span className="eyebrow">DEVICE-BOUND TERMINAL</span><h1>控制台</h1><p>{activeDevice.name}</p></div></div><Panel><EmptyState icon={<ShieldAlert size={30} />} title="设备当前不可操作" detail="请先连接设备并完成调试授权。" /></Panel></div>;

  if (activeDevice.platform !== "android") return <div className="page console-page"><div className="page-heading"><div><span className="eyebrow">DEVICE-BOUND TERMINAL</span><h1>控制台</h1><p>{activeDevice.name}</p></div></div><Panel><EmptyState icon={<Smartphone size={30} />} title="iOS 交互式控制台尚未开放" detail="MVP 将 iOS 能力限制为明确的 libimobiledevice 动作；后续可加入 syslog 和 Frida REPL 会话。" /></Panel></div>;

  const visibleEntries = entries.filter((entry) => !filter || `${entry.command} ${entry.output} ${entry.error ?? ""}`.toLowerCase().includes(filter.toLowerCase()));

  return (
    <div className="page console-page">
      <div className="page-heading">
        <div><span className="eyebrow">DEVICE-BOUND TERMINAL</span><h1>控制台</h1><p>只执行当前 Android 设备上的 ADB Shell 命令，不调用本机 Shell。</p></div>
        <div className="heading-actions"><Button icon={<Download size={15} />} disabled={!entries.length} onClick={exportOutput}>导出输出</Button><Button icon={<Eraser size={15} />} onClick={() => setEntries([])} disabled={!entries.length}>清屏</Button></div>
      </div>
      <InlineNotice tone="warning" title="高级能力">设备 Shell 等同于直接控制当前设备。请核对目标和命令，只用于你拥有或获准测试的设备。</InlineNotice>

      <div className="terminal-window">
        <header className="terminal-tabs">
          <div className="terminal-tab active"><TerminalSquare size={14} /><span>ADB Shell · {activeDevice.name}</span><StatusDot status={activeDevice.state === "online" ? "success" : "muted"} /></div>
          <div className="terminal-tools"><div className="search-input small"><Search size={14} /><input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="搜索输出" /></div><button className="icon-button" title="复制全部" aria-label="复制全部控制台输出" disabled={!entries.length} onClick={() => void copyAll()}><Copy size={14} /></button></div>
        </header>
        <div className="terminal-context">
          <div><Smartphone size={14} /><span>目标</span><strong>{activeDevice.name}</strong><code>{activeDevice.id}</code></div>
          <div><ShieldAlert size={14} /><span>权限</span><StatusBadge tone={activeDevice.rooted ? "purple" : "neutral"}>{activeDevice.rooted ? "Root 可用" : "Shell"}</StatusBadge></div>
        </div>
        <main className="terminal-output">
          <div className="terminal-welcome"><span>Mobius Device Console</span><small>会话已绑定 {activeDevice.id} · 输入仅作为 adb shell 的远端参数发送</small></div>
          {visibleEntries.map((entry) => <div className="terminal-entry" key={entry.id}>
            <div className="terminal-command"><span>{activeDevice.rooted ? "#" : "$"}</span><code>{entry.command}</code><time>{entry.startedAt}</time></div>
            {entry.running ? <div className="terminal-running"><Circle className="pulse" size={8} fill="currentColor" />等待设备响应…</div> : <><pre>{entry.output}</pre>{entry.error && <pre className="terminal-error">{entry.error}</pre>}<span className="terminal-duration">完成 · {entry.duration} ms</span></>}
          </div>)}
          {!visibleEntries.length && filter && <div className="terminal-empty">没有匹配的输出</div>}
          <div ref={endRef} />
        </main>
        <div className="terminal-suggestions">{suggestedCommands.map((item) => <button key={item.command} disabled={running} title={`立即执行：${item.command}`} onClick={() => void runCommand(item.command)}>{item.label}</button>)}</div>
        <form className="terminal-input" onSubmit={execute}>
          <span className="terminal-prompt">{activeDevice.rooted ? "#" : "$"}</span>
          <input value={command} onChange={(e) => setCommand(e.target.value)} onKeyDown={historyKey} placeholder="输入设备 Shell 命令…" autoCapitalize="off" autoCorrect="off" spellCheck={false} disabled={running} />
          <Button type="submit" variant="primary" icon={<Play size={14} fill="currentColor" />} disabled={!command.trim() || running}>{running ? "运行中" : "执行"}</Button>
        </form>
      </div>
      <div className="console-footnote"><Code2 size={14} /><span>上方预设是固定的只读查询，点击即执行；自定义命令仍会按风险提示确认。Mobius 不会把输入拼接到本机 Shell。</span></div>
      {pendingCommand && <Modal title="确认执行设备写入命令" subtitle={`目标：${activeDevice.name} · ${activeDevice.id}`} onClose={() => setPendingCommand(undefined)} footer={<><Button onClick={() => setPendingCommand(undefined)}>取消</Button><Button variant="danger" onClick={() => { const next = pendingCommand; setPendingCommand(undefined); void runCommand(next); }}>确认并执行</Button></>}><div className="form-stack"><InlineNotice tone="danger" title="该命令不在只读白名单">它可能修改设备状态、写入文件、停止进程或重启设备。请核对目标与完整命令。</InlineNotice><div className="command-preview"><span>将在设备 Shell 中执行</span><code>{pendingCommand}</code></div></div></Modal>}
    </div>
  );
}
