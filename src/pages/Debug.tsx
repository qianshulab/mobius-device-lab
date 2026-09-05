import { Activity, CheckCircle2, ChevronRight, CircleStop, Clipboard, Code2, Cpu, FileUp, KeyRound, Link2, LoaderCircle, Play, Power, RefreshCw, RotateCcw, ScrollText, Smartphone, TerminalSquare, Trash2, Wrench } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { chooseBinaryFile } from "../lib/dialog";
import { Button, EmptyState, InlineNotice, Modal, Panel, StatusBadge, StatusDot, Tabs } from "../components/Ui";
import type { ActivityItem, Device, FridaServerResult, IosDeviceAction, IosDeviceActionConfirmation, IosDiagnosticKind, IosDiagnosticToolStatus, IosFridaServerResult, IosHostDiagnosticKind, IosSshSession, ToastMessage } from "../types";
import ConsolePage from "./Console";

export type DebugView = "instrumentation" | "system" | "shell";
type IosToolSource = "host" | "ssh";
type SystemBusyKey = "properties" | "processes" | "reboot" | "ios-overview" | "ios-processes" | "ios-tools" | "ios-syslog" | "ios-respring" | "ios-reboot" | `ios-host-${IosHostDiagnosticKind}`;

interface DebugProps {
  activeDevice?: Device;
  initialView?: DebugView;
  initialCommand?: string;
  onRefresh: () => void;
  onAction: (action: "frida") => void;
  androidFridaResult?: FridaServerResult;
  onAndroidFridaResultChange: (result?: FridaServerResult) => void;
  iosSession?: IosSshSession;
  iosFridaResult?: IosFridaServerResult;
  onIosFridaResultChange: (result?: IosFridaServerResult) => void;
  onOpenIosFiles: () => void;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}

export default function DebugPage({ activeDevice, initialView = "instrumentation", initialCommand = "", onRefresh, onAction, androidFridaResult, onAndroidFridaResultChange, iosSession, iosFridaResult, onIosFridaResultChange, onOpenIosFiles, notify, record }: DebugProps) {
  const [view, setView] = useState<DebugView>(initialView);
  const [iosToolSource, setIosToolSource] = useState<IosToolSource>(activeDevice?.connectionSource === "manual" ? "ssh" : "host");
  const [systemBusy, setSystemBusy] = useState<SystemBusyKey>();
  const [rebootConfirm, setRebootConfirm] = useState(false);
  const [iosActionConfirm, setIosActionConfirm] = useState<IosDeviceActionConfirmation>();
  const [androidFridaBusy, setAndroidFridaBusy] = useState(false);
  const [systemResult, setSystemResult] = useState<{ label: string; command: string; output: string; error?: string; duration?: number; at: string; running?: boolean; source?: string; tools?: IosDiagnosticToolStatus[]; warnings?: string[] }>();
  const systemRequest = useRef(0);
  const androidReady = activeDevice?.platform === "android" && activeDevice.state === "online";
  const rootedAndroid = androidReady && !!activeDevice?.rooted;
  const iosReady = activeDevice?.platform === "ios" && !!iosSession?.connected && !!iosSession.jailbreakConfirmed;
  const iosRootReady = iosReady && iosSession.remoteUid === 0;
  const iosHostReady = activeDevice?.platform === "ios"
    && activeDevice.state === "online"
    && activeDevice.connectionSource !== "manual"
    && !activeDevice.id.startsWith("ios-ssh:");
  const iosHostNetwork = iosHostReady && activeDevice.transport === "wifi";
  const visibleView = activeDevice?.platform === "ios" && view === "shell" ? "system" : view;
  const tabOptions: Array<{ id: DebugView; label: string }> = activeDevice?.platform === "ios"
    ? [{ id: "instrumentation", label: "Frida" }, { id: "system", label: "iOS 工具" }]
    : [{ id: "instrumentation", label: "Frida" }, { id: "system", label: "系统与进程" }, { id: "shell", label: "Shell" }];

  useEffect(() => {
    if (activeDevice?.platform !== "ios") return;
    setIosToolSource(activeDevice.connectionSource === "manual" || activeDevice.id.startsWith("ios-ssh:") ? "ssh" : "host");
  }, [activeDevice?.connectionSource, activeDevice?.id, activeDevice?.platform]);

  useEffect(() => {
    systemRequest.current += 1;
    setSystemResult(undefined);
    setSystemBusy(undefined);
    setRebootConfirm(false);
    setIosActionConfirm(undefined);
  }, [activeDevice?.id, iosSession?.sessionId, iosSession?.sshHost, iosSession?.sshPort, iosSession?.username, iosSession?.serverSystem]);

  useEffect(() => {
    if (activeDevice?.platform === "ios" && view === "shell") setView("system");
  }, [activeDevice?.platform, view]);

  useEffect(() => {
    if (!iosActionConfirm) return;
    const timer = window.setTimeout(
      () => setIosActionConfirm((current) => current?.confirmationId === iosActionConfirm.confirmationId ? undefined : current),
      iosActionConfirm.expiresInSeconds * 1000,
    );
    return () => window.clearTimeout(timer);
  }, [iosActionConfirm]);

  const runSystemAction = async (id: "properties" | "processes" | "reboot", label: string, command: string) => {
    if (!androidReady || !activeDevice || systemBusy) return;
    const target = activeDevice;
    const request = ++systemRequest.current;
    const started = performance.now();
    setSystemBusy(id);
    setSystemResult({ label, command, output: "", at: new Date().toLocaleTimeString("zh-CN"), running: true });
    record(`开始${label}`, `${target.name} · ${command}`, "info");
    try {
      const result = await api.shell(target.id, command);
      const duration = Math.round(performance.now() - started);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command, output: result.stdout?.trim() || result.message, error: result.success ? result.stderr?.trim() || undefined : result.message, duration, at: new Date().toLocaleTimeString("zh-CN") });
      if (!result.success) {
        notify("error", `${label}失败`, result.message);
        record(`${label}失败`, `${target.name} · ${result.message}`, "error");
      } else {
        notify("success", label === "重启设备" ? "重启命令已发送" : `${label}已刷新`, `${duration} ms`);
        record(`${label}完成`, `${target.name} · ${duration} ms`);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command, output: "", error: message, duration: Math.round(performance.now() - started), at: new Date().toLocaleTimeString("zh-CN") });
      notify("error", `${label}失败`, message);
      record(`${label}失败`, `${target.name} · ${message}`, "error");
    } finally {
      if (systemRequest.current === request) setSystemBusy(undefined);
    }
  };

  const runIosHostDiagnostic = async (kind: IosHostDiagnosticKind, label: string) => {
    if (!iosHostReady || !activeDevice || systemBusy) {
      if (activeDevice?.platform === "ios" && !iosHostReady) notify("warning", "主机工具当前不可用", "请选择一台由 libimobiledevice 发现的在线 USB 或 Wi-Fi iOS 设备。");
      return;
    }
    const target = activeDevice;
    const network = iosHostNetwork;
    const command = kind === "deviceInfo"
      ? "ideviceinfo"
      : kind === "pairing"
        ? "idevicepair validate"
        : kind === "apps"
          ? "ideviceinstaller list --all"
          : "idevicesyslog · 5 秒采样";
    const request = ++systemRequest.current;
    const started = performance.now();
    const busyKey: SystemBusyKey = `ios-host-${kind}`;
    setSystemBusy(busyKey);
    setSystemResult({ label, command: `主机工具 · ${command}`, output: "", at: new Date().toLocaleTimeString("zh-CN"), running: true, source: network ? "libimobiledevice · Wi-Fi" : "libimobiledevice · USB" });
    record(`开始${label}`, `${target.name} · ${command} · ${network ? "Wi-Fi" : "USB"}`, "info");
    try {
      const result = await api.iosHostDiagnostic(target.id, kind, network);
      const extended = result as typeof result & { udid?: string; tool?: string; stderr?: string; durationMs?: number };
      if (extended.udid && extended.udid !== target.id) throw new Error("主机工具返回的设备与已锁定目标不一致。");
      if (result.kind !== kind) throw new Error("主机工具返回的诊断类型与请求不一致。");
      const duration = extended.durationMs ?? Math.round(performance.now() - started);
      const warnings = [...(Array.isArray(result.warnings) ? result.warnings : [])];
      if (extended.stderr?.trim()) warnings.push(`工具提示：${extended.stderr.trim()}`);
      if (result.truncated && !warnings.some((warning) => warning.includes("截断"))) warnings.push("输出已在安全上限处截断。");
      if (systemRequest.current !== request) return;
      const source = result.source || extended.tool || command;
      setSystemResult({ label: result.title || label, command: `主机工具 · ${command}`, output: result.output, duration, at: new Date().toLocaleTimeString("zh-CN"), source, warnings });
      notify(warnings.length ? "warning" : "success", `${result.title || label}已刷新`, `${duration} ms · ${source}`);
      record(`${result.title || label}完成`, `${target.name} · ${source} · ${duration} ms${result.truncated ? " · 输出已截断" : ""}`, warnings.length ? "info" : "success");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command: `主机工具 · ${command}`, output: "", error: message, duration: Math.round(performance.now() - started), at: new Date().toLocaleTimeString("zh-CN"), source: network ? "libimobiledevice · Wi-Fi" : "libimobiledevice · USB" });
      notify("error", `${label}失败`, message);
      record(`${label}失败`, `${target.name} · ${command} · ${message}`, "error");
    } finally {
      if (systemRequest.current === request) setSystemBusy(undefined);
    }
  };

  const runIosDiagnostic = async (kind: IosDiagnosticKind, label: string) => {
    if (!iosReady || !activeDevice || !iosSession || systemBusy) {
      if (activeDevice?.platform === "ios" && !iosReady) notify("warning", "请先连接 iOS SSH", "连接后即可直接读取固定诊断项目。");
      return;
    }
    const target = activeDevice;
    const request = ++systemRequest.current;
    const started = performance.now();
    const busyKey = `ios-${kind}` as const;
    setSystemBusy(busyKey);
    setSystemResult({ label, command: `iOS 固定只读诊断 · ${label}`, output: "", at: new Date().toLocaleTimeString("zh-CN"), running: true });
    record(`开始读取${label}`, `${target.name} · SSH 会话绑定`, "info");
    try {
      const result = await api.iosRuntimeSnapshot(iosSession.sessionId, kind, 120);
      const duration = Math.round(performance.now() - started);
      if (systemRequest.current !== request) return;
      setSystemResult({ label: result.title, command: `iOS 固定只读诊断 · ${label}`, output: result.output, duration, at: new Date().toLocaleTimeString("zh-CN"), source: result.source, tools: result.tools, warnings: result.warnings });
      notify(result.warnings.length ? "warning" : "success", `${result.title}已刷新`, `${duration} ms · ${result.source}`);
      record(`${result.title}完成`, `${target.name} · ${duration} ms${result.truncated ? " · 输出已截断" : ""}`, result.warnings.length ? "info" : "success");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command: `iOS 固定只读诊断 · ${label}`, output: "", error: message, duration: Math.round(performance.now() - started), at: new Date().toLocaleTimeString("zh-CN") });
      notify("error", `${label}读取失败`, message);
      record(`${label}读取失败`, `${target.name} · ${message}`, "error");
    } finally {
      if (systemRequest.current === request) setSystemBusy(undefined);
    }
  };

  const prepareIosAction = async (action: IosDeviceAction) => {
    if (!iosRootReady || !iosSession || systemBusy) return;
    const request = ++systemRequest.current;
    setSystemBusy(action === "respring" ? "ios-respring" : "ios-reboot");
    try {
      const confirmation = await api.prepareIosDeviceAction(iosSession, action);
      if (systemRequest.current !== request) return;
      setIosActionConfirm(confirmation);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (systemRequest.current !== request) return;
      notify("error", "无法准备设备操作", message);
    } finally {
      if (systemRequest.current === request) setSystemBusy(undefined);
    }
  };

  const runIosAction = async (confirmation: IosDeviceActionConfirmation) => {
    if (!iosRootReady || !activeDevice || !iosSession || systemBusy) return;
    if (confirmation.sessionId !== iosSession.sessionId
      || confirmation.target.sshHost !== iosSession.sshHost
      || confirmation.target.sshPort !== iosSession.sshPort
      || confirmation.target.username !== iosSession.username
      || confirmation.target.serverSystem !== iosSession.serverSystem) {
      notify("warning", "SSH 目标已变化", "请重新核对当前 SSH 目标后再操作。");
      return;
    }
    const target = activeDevice;
    const request = ++systemRequest.current;
    const action = confirmation.action;
    const label = action === "respring" ? "Respring" : "重启设备";
    const started = performance.now();
    setSystemBusy(action === "respring" ? "ios-respring" : "ios-reboot");
    setSystemResult({ label, command: `iOS 固定操作 · ${label}`, output: "", at: new Date().toLocaleTimeString("zh-CN"), running: true });
    record(`开始${label}`, `${target.name} · 已确认固定操作`, "info");
    try {
      const result = await api.runIosDeviceAction(confirmation);
      const duration = Math.round(performance.now() - started);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command: `iOS 固定操作 · ${label}`, output: result.message, duration, at: new Date().toLocaleTimeString("zh-CN"), source: "Root SSH 固定动作" });
      notify("success", `${label}已调度`, action === "reboot" ? "设备重启后请重新连接 SSH。" : "SpringBoard 重新载入期间界面会短暂不可用。");
      record(`${label}已调度`, `${confirmation.target.username}@${confirmation.target.sshHost}:${confirmation.target.sshPort} · ${duration} ms`);
      window.setTimeout(onRefresh, 1200);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (systemRequest.current !== request) return;
      setSystemResult({ label, command: `iOS 固定操作 · ${label}`, output: "", error: message, duration: Math.round(performance.now() - started), at: new Date().toLocaleTimeString("zh-CN") });
      notify("error", `${label}失败`, message);
      record(`${label}失败`, `${target.name} · ${message}`, "error");
    } finally {
      if (systemRequest.current === request) setSystemBusy(undefined);
    }
  };

  const copySystemResult = async () => {
    if (!systemResult) return;
    try {
      const toolText = systemResult.tools?.map((tool) => `${tool.available ? "[可用]" : "[未检测到]"}${tool.running === true ? "[运行中]" : tool.running === false ? "[未运行]" : ""} ${tool.name}${tool.path ? ` · ${tool.path}` : ""}${tool.version ? ` · ${tool.version}` : ""}`).join("\n");
      await navigator.clipboard.writeText([systemResult.command, systemResult.source, systemResult.output, toolText, systemResult.warnings?.join("\n"), systemResult.error].filter(Boolean).join("\n"));
      notify("success", "结果已复制");
    } catch {
      notify("warning", "无法复制结果", "请直接在结果区域中选择文本复制。");
    }
  };

  const clearSystemResult = () => {
    systemRequest.current += 1;
    setSystemResult(undefined);
    setSystemBusy(undefined);
  };

  const stopAndroidFrida = async () => {
    const target = activeDevice;
    if (!target || target.platform !== "android" || !androidFridaResult?.active || androidFridaBusy) return;
    setAndroidFridaBusy(true);
    try {
      const stopped = await api.stopFrida(target.id);
      if (!stopped.success) throw new Error(stopped.message);
      onAndroidFridaResultChange(undefined);
      notify("success", "Frida Server 已停止", `已清理 ${target.name} 的托管进程与 ADB Forward。`);
      record("停止 Frida Server", `${target.name} · ${stopped.remotePath}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "Frida 停止失败", message);
      record("Frida 停止失败", `${target.name} · ${message}`, "error");
    } finally {
      setAndroidFridaBusy(false);
    }
  };

  return <div className="page debug-page">
    <div className="page-heading"><div><span className="eyebrow">RUNTIME & DEVICE DEBUG</span><h1>调试</h1><p>{activeDevice?.platform === "ios" ? "Frida、libimobiledevice 主机工具与越狱 SSH 固定诊断；无需手工输入命令。" : "Frida 动态插桩、设备状态与绑定目标的 Shell。"}</p></div><Button icon={<RefreshCw size={14} />} onClick={onRefresh}>刷新能力</Button></div>
    <Tabs value={visibleView} onChange={setView} options={tabOptions} />

    {!activeDevice && visibleView !== "shell" && <Panel><EmptyState icon={<Smartphone size={29} />} title="请先选择设备" detail="调试操作始终绑定一台明确的目标设备；你仍可在设置中检查主机工具链。" /></Panel>}

    {activeDevice?.platform === "android" && visibleView === "instrumentation" && <div className="debug-grid">
      <Panel className="span-7" title={<><KeyRound size={17} /> Frida Server 管理</>}>
        <div className="frida-profile-hero"><span className="quick-icon quick-purple"><KeyRound size={23} /></span><div><h2>版本化 Server 配置</h2><p>预置 16.1.4 与最新稳定版 17.17.0 配置槽；二进制始终由你从本机选择，不随应用分发。</p></div></div>
        <div className="frida-profile-list"><div><StatusDot status="muted" /><span><strong>Frida 16.1.4</strong><small>兼容旧客户端与现有脚本环境</small></span><StatusBadge>用户文件</StatusBadge></div><div><StatusDot status="muted" /><span><strong>Frida 17.17.0</strong><small>最新稳定配置 · 2026-08-05</small></span><StatusBadge tone="purple">LATEST</StatusBadge></div><div><StatusDot status="muted" /><span><strong>自定义版本</strong><small>记录版本、ABI 与本地文件路径</small></span><StatusBadge>可新增</StatusBadge></div></div>
        <Button variant="primary" icon={androidFridaResult?.active ? <KeyRound size={14} /> : <Play size={14} />} disabled={!rootedAndroid} onClick={() => onAction("frida")}>{androidFridaResult?.active ? "查看当前会话" : "选择配置并启动"}</Button>
      </Panel>
      <Panel className="span-5" title="当前会话资源">
        <div className="resource-row"><div><StatusDot status={androidFridaResult?.active ? "running" : "muted"} /><span><strong>{androidFridaResult?.active ? `PID ${androidFridaResult.pid ?? "—"}` : "Server"}</strong><small>{androidFridaResult?.active ? `${androidFridaResult.listenAddress ?? "127.0.0.1"}:${androidFridaResult.hostPort ?? "—"} → 设备 ${androidFridaResult.devicePort ?? "—"}` : "尚未由 Mobius 启动"}</small></span></div><Button variant="ghost" icon={androidFridaBusy ? <LoaderCircle className="spin" size={14} /> : <CircleStop size={14} />} disabled={!androidFridaResult?.active || androidFridaBusy} onClick={() => void stopAndroidFrida()}>停止并清理</Button></div>
        {androidFridaResult?.active && <div className="debug-facts"><div><Code2 size={14} /><span><strong>{androidFridaResult.remotePath}</strong><small>设备端中性路径</small></span></div><div><Link2 size={14} /><span><strong>{androidFridaResult.mapping?.local ?? `tcp:${androidFridaResult.hostPort ?? "—"}`}</strong><small>ADB Forward → {androidFridaResult.mapping?.remote ?? `tcp:${androidFridaResult.devicePort ?? "—"}`}</small></span></div></div>}
        <div className="debug-policy"><Code2 size={17} /><div><strong>远端中性别名，可完整审计</strong><p>上传名不含工具关键字；界面仍显示真实路径、版本、PID、设备监听端口和主机转发端口。</p></div></div>
        <div className="debug-policy"><KeyRound size={17} /><div><strong>精确进程归属</strong><p>停止前核验 PID、可执行路径和启动时间，退出应用时只清理由本会话创建的资源。</p></div></div>
      </Panel>
    </div>}

    {activeDevice?.platform === "ios" && visibleView === "instrumentation" && <IosFridaWorkspace
      device={activeDevice}
      session={iosSession}
      result={iosFridaResult}
      onResultChange={onIosFridaResultChange}
      onOpenFiles={onOpenIosFiles}
      notify={notify}
      record={record}
    />}

    {activeDevice && visibleView === "system" && <div className="debug-grid">
      <Panel className="span-4" title={<>{activeDevice.platform === "ios" ? <Wrench size={17} /> : <Cpu size={17} />} {activeDevice.platform === "ios" ? "iOS 工具" : "系统快捷操作"}</>}>
        {activeDevice.platform === "ios" && <div className="ios-diagnostic-connect">
          <Tabs value={iosToolSource} onChange={setIosToolSource} options={[{ id: "host", label: "主机 · libimobiledevice" }, { id: "ssh", label: "越狱设备 · SSH" }]} />
          {iosToolSource === "host" && !iosHostReady && <><InlineNotice tone="warning" title="主机工具需要已配对的 iOS 连接">{activeDevice.connectionSource === "manual" ? "当前是手工登记的 SSH 端点，没有可供 libimobiledevice 使用的 UDID。" : "请通过 USB 或已配对的 Wi-Fi 连接设备后重试。"}</InlineNotice><Button icon={<Link2 size={14} />} onClick={() => setIosToolSource("ssh")}>切换到 SSH 工具</Button></>}
          {iosToolSource === "ssh" && !iosReady && <><InlineNotice tone="warning" title="先建立 SSH 会话">主机工具仍可独立使用；连接越狱设备后可执行固定的 SSH 诊断与设备操作。</InlineNotice><Button variant="primary" icon={<Link2 size={14} />} onClick={onOpenIosFiles}>连接 SSH</Button></>}
        </div>}
        <div className="debug-action-list compact-actions">
          {activeDevice.platform === "android" ? <>
            <button disabled={!androidReady || !!systemBusy} onClick={() => void runSystemAction("properties", "系统属性", "getprop")}><span className="quick-icon quick-green">{systemBusy === "properties" ? <LoaderCircle className="spin" size={19} /> : <Cpu size={19} />}</span><span><strong>系统属性</strong><small>点击执行，结果留在当前页面</small></span><ChevronRight size={15} /></button>
            <button disabled={!androidReady || !!systemBusy} onClick={() => void runSystemAction("processes", "进程列表", "ps -A")}><span className="quick-icon quick-blue">{systemBusy === "processes" ? <LoaderCircle className="spin" size={19} /> : <Activity size={19} />}</span><span><strong>进程列表</strong><small>点击获取当前进程快照</small></span><ChevronRight size={15} /></button>
            <button disabled={!androidReady || !!systemBusy} onClick={() => setRebootConfirm(true)}><span className="quick-icon quick-rose">{systemBusy === "reboot" ? <LoaderCircle className="spin" size={19} /> : <RotateCcw size={19} />}</span><span><strong>重启设备</strong><small>确认目标后直接发送重启命令</small></span><ChevronRight size={15} /></button>
          </> : iosToolSource === "host" ? <>
            <div className="ios-system-action-label">主机侧固定工具 · 不需要 SSH</div>
            <button disabled={!iosHostReady || !!systemBusy} onClick={() => void runIosHostDiagnostic("deviceInfo", "设备信息")}><span className="quick-icon quick-green">{systemBusy === "ios-host-deviceInfo" ? <LoaderCircle className="spin" size={19} /> : <Cpu size={19} />}</span><span><strong>设备信息</strong><small>ideviceinfo · 版本、型号与设备属性</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosHostReady || !!systemBusy} onClick={() => void runIosHostDiagnostic("pairing", "配对状态")}><span className="quick-icon quick-blue">{systemBusy === "ios-host-pairing" ? <LoaderCircle className="spin" size={19} /> : <CheckCircle2 size={19} />}</span><span><strong>配对状态</strong><small>idevicepair validate · 只验证，不修改配对</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosHostReady || !!systemBusy} onClick={() => void runIosHostDiagnostic("apps", "已安装应用")}><span className="quick-icon quick-purple">{systemBusy === "ios-host-apps" ? <LoaderCircle className="spin" size={19} /> : <Smartphone size={19} />}</span><span><strong>已安装应用</strong><small>ideviceinstaller · 读取全部应用清单</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosHostReady || !!systemBusy} onClick={() => void runIosHostDiagnostic("syslog", "系统日志采样")}><span className="quick-icon quick-orange">{systemBusy === "ios-host-syslog" ? <LoaderCircle className="spin" size={19} /> : <ScrollText size={19} />}</span><span><strong>系统日志采样</strong><small>idevicesyslog · 5 秒后自动停止</small></span><ChevronRight size={15} /></button>
          </> : <>
            <div className="ios-system-action-label">越狱设备固定诊断 · SSH</div>
            <button disabled={!iosReady || !!systemBusy} onClick={() => void runIosDiagnostic("overview", "设备概览")}><span className="quick-icon quick-green">{systemBusy === "ios-overview" ? <LoaderCircle className="spin" size={19} /> : <Cpu size={19} />}</span><span><strong>设备概览</strong><small>版本、身份、磁盘与内存</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosReady || !!systemBusy} onClick={() => void runIosDiagnostic("processes", "进程快照")}><span className="quick-icon quick-blue">{systemBusy === "ios-processes" ? <LoaderCircle className="spin" size={19} /> : <Activity size={19} />}</span><span><strong>进程快照</strong><small>固定上限的即时进程列表</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosReady || !!systemBusy} onClick={() => void runIosDiagnostic("tools", "设备端工具")}><span className="quick-icon quick-purple">{systemBusy === "ios-tools" ? <LoaderCircle className="spin" size={19} /> : <Wrench size={19} />}</span><span><strong>设备端工具</strong><small>Frida、debugserver、dpkg 等固定路径状态</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosReady || !!systemBusy} onClick={() => void runIosDiagnostic("syslog", "最近日志")}><span className="quick-icon quick-orange">{systemBusy === "ios-syslog" ? <LoaderCircle className="spin" size={19} /> : <ScrollText size={19} />}</span><span><strong>最近系统日志</strong><small>最近 5 分钟，最多 120 行</small></span><ChevronRight size={15} /></button>
            <div className="ios-system-action-label">需确认的设备操作</div>
            <button disabled={!iosRootReady || !!systemBusy} onClick={() => void prepareIosAction("respring")}><span className="quick-icon quick-orange">{systemBusy === "ios-respring" ? <LoaderCircle className="spin" size={19} /> : <RotateCcw size={19} />}</span><span><strong>Respring</strong><small>核对 SSH 目标后重新载入</small></span><ChevronRight size={15} /></button>
            <button disabled={!iosRootReady || !!systemBusy} onClick={() => void prepareIosAction("reboot")}><span className="quick-icon quick-rose">{systemBusy === "ios-reboot" ? <LoaderCircle className="spin" size={19} /> : <Power size={19} />}</span><span><strong>重启设备</strong><small>核对 Root SSH 目标后执行</small></span><ChevronRight size={15} /></button>
          </>}
        </div>
      </Panel>
      <Panel className="span-8 system-result-panel" title={<><TerminalSquare size={17} /> 执行结果</>} action={systemResult && <div className="panel-button-row"><Button variant="ghost" icon={<Clipboard size={13} />} onClick={() => void copySystemResult()}>复制</Button><Button variant="ghost" icon={<Trash2 size={13} />} disabled={systemBusy === "reboot" || systemBusy === "ios-respring" || systemBusy === "ios-reboot"} onClick={clearSystemResult}>清空</Button></div>}>
        {!systemResult ? <EmptyState icon={<TerminalSquare size={27} />} title="选择左侧操作" detail={activeDevice.platform === "ios" ? iosToolSource === "host" ? "主机侧固定工具不依赖 SSH，输出会在这里就地显示。" : "越狱 SSH 固定诊断会立即执行并就地显示。" : "确定性的只读命令会立即执行，结果直接显示在这里，不再跳转 Shell。"} /> : <div className="system-result">
          <header><div><StatusDot status={systemResult.running ? "running" : systemResult.error ? "error" : "success"} /><strong>{systemResult.label}</strong><code>{systemResult.command}</code></div><span>{systemResult.running ? "执行中…" : `${systemResult.at}${systemResult.duration !== undefined ? ` · ${systemResult.duration} ms` : ""}`}</span></header>
          {systemResult.running ? <div className="system-result-loading"><LoaderCircle className="spin" size={18} />正在读取设备数据…</div> : <div className="system-result-body">
            {systemResult.source && <div className="system-result-source"><StatusDot status="info" />{systemResult.source}</div>}
            {systemResult.tools?.length ? <div className="ios-tool-status-grid">{systemResult.tools.map((tool) => <div key={tool.id} className={tool.available ? "tool-available" : "tool-missing"}><StatusDot status={tool.running === true ? "running" : tool.available ? "success" : "muted"} /><span><strong>{tool.name}</strong><small>{tool.path ?? "未检测到"}</small>{tool.version && <code>{tool.version}</code>}{tool.running !== undefined && <em>{tool.running ? "服务正在运行" : "已安装，当前未运行"}</em>}</span><StatusBadge tone={tool.running === true ? "purple" : tool.available ? "success" : "neutral"}>{tool.running === true ? "运行中" : tool.available ? "可用" : "缺失"}</StatusBadge></div>)}</div> : <pre>{systemResult.output || "操作已完成，没有返回文本。"}</pre>}
            {systemResult.warnings?.map((warning) => <div key={warning} className="system-result-warning">{warning}</div>)}
            {systemResult.error && <pre className="system-result-error">{systemResult.error}</pre>}
          </div>}
        </div>}
      </Panel>
      <Panel className="span-12 system-target-strip"><div className="system-target">
        {activeDevice.platform === "ios" && iosToolSource === "host" ? <>
          <div><StatusDot status={iosHostReady ? "success" : "warning"} /><span><strong>{activeDevice.name}</strong><small>{activeDevice.id}</small></span></div>
          <div><StatusDot status={iosHostReady ? "success" : "warning"} /><span><strong>{iosHostReady ? `libimobiledevice · ${iosHostNetwork ? "Wi-Fi" : "USB"}` : "主机通道不可用"}</strong><small>{iosHostReady ? "固定参数直接绑定当前 UDID，不需要 SSH" : "可切换到越狱 SSH 工具"}</small></span></div>
        </> : <>
          <div><StatusDot status={activeDevice.platform === "ios" ? iosReady ? "success" : "warning" : activeDevice.state === "online" ? "success" : "warning"} /><span><strong>{activeDevice.platform === "ios" && iosSession ? `${iosSession.username}@${iosSession.sshHost}:${iosSession.sshPort}` : activeDevice.name}</strong><small>{activeDevice.platform === "ios" && iosSession ? iosSession.serverSystem ?? `SSH 会话 ${iosSession.sessionId.slice(0, 12)}…` : activeDevice.id}</small></span></div>
          <div><CheckCircle2 size={15} /><span><strong>{activeDevice.platform === "ios" ? iosRootReady ? "iOS Root SSH" : iosReady ? "iOS SSH 已验证" : "等待 SSH" : activeDevice.rooted ? "Android Root" : "标准权限"}</strong><small>{activeDevice.platform === "ios" ? "操作绑定实际 SSH 端点与当前会话" : "所有操作固定绑定此设备与当前会话"}</small></span></div>
        </>}
      </div></Panel>
      {rebootConfirm && <Modal title="确认重启设备" subtitle={`${activeDevice.name} · ${activeDevice.id}`} onClose={() => setRebootConfirm(false)} footer={<><Button onClick={() => setRebootConfirm(false)}>取消</Button><Button variant="danger" icon={<RotateCcw size={14} />} onClick={() => { setRebootConfirm(false); void runSystemAction("reboot", "重启设备", "reboot"); }}>确认并重启</Button></>}><InlineNotice tone="danger" title="设备连接将暂时中断">Mobius 会直接向当前选中的 Android 设备发送重启命令；未保存的设备端工作可能丢失。</InlineNotice></Modal>}
      {iosActionConfirm && <Modal title={iosActionConfirm.action === "respring" ? "确认 Respring" : "确认重启 iPhone"} subtitle={`${iosActionConfirm.target.username}@${iosActionConfirm.target.sshHost}:${iosActionConfirm.target.sshPort}`} onClose={() => setIosActionConfirm(undefined)} footer={<><Button onClick={() => setIosActionConfirm(undefined)}>取消</Button><Button variant={iosActionConfirm.action === "reboot" ? "danger" : "primary"} icon={iosActionConfirm.action === "reboot" ? <Power size={14} /> : <RotateCcw size={14} />} onClick={() => { const confirmation = iosActionConfirm; setIosActionConfirm(undefined); void runIosAction(confirmation); }}>{iosActionConfirm.action === "reboot" ? "确认并重启" : "确认 Respring"}</Button></>}><div className="ios-action-target-confirm"><strong>实际 SSH 目标</strong><code>{iosActionConfirm.target.username}@{iosActionConfirm.target.sshHost}:{iosActionConfirm.target.sshPort}</code><small>{iosActionConfirm.target.serverSystem ?? "设备系统信息未返回"}</small><small>SSH 主机标识：{iosActionConfirm.target.hostKeyIdentity}</small><small>一次性确认在 {iosActionConfirm.expiresInSeconds} 秒后失效</small></div><InlineNotice tone={iosActionConfirm.action === "reboot" ? "danger" : "warning"} title={iosActionConfirm.action === "reboot" ? "SSH 连接将中断" : "SpringBoard 将短暂重载"}>{iosActionConfirm.action === "reboot" ? "设备重启后需要重新建立 SSH 会话；请先保存设备端工作。" : "仅调用固定的 sbreload 或 SpringBoard 重载动作。"}</InlineNotice></Modal>}
    </div>}

    {visibleView === "shell" && <div className="debug-console"><ConsolePage activeDevice={activeDevice} initialCommand={initialCommand} notify={notify} record={record} /></div>}
  </div>;
}

type FridaProfile = "16.1.4" | "17.17.0" | "custom";

function validPort(value: string, optional = false) {
  if (optional && !value.trim()) return undefined;
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 && parsed <= 65535 ? parsed : null;
}

function IosFridaWorkspace({ device, session, result, onResultChange, onOpenFiles, notify, record }: {
  device: Device;
  session?: IosSshSession;
  result?: IosFridaServerResult;
  onResultChange: (result?: IosFridaServerResult) => void;
  onOpenFiles: () => void;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}) {
  const [profile, setProfile] = useState<FridaProfile>("16.1.4");
  const [customVersion, setCustomVersion] = useState("");
  const [localPath, setLocalPath] = useState("");
  const [devicePort, setDevicePort] = useState("27042");
  const [hostPort, setHostPort] = useState("");
  const [busy, setBusy] = useState<"choose" | "start" | "stop">();
  const currentResult = result?.sessionId === session?.sessionId ? result : undefined;
  const verifiedRootSession = !!session?.connected && !!session.jailbreakConfirmed && session.remoteUid === 0;
  const selectedVersion = profile === "custom" ? customVersion.trim() || "自定义" : profile;
  const storageKey = `mobius.frida.ios.${profile}.${device.architecture ?? "arm64"}`;

  useEffect(() => {
    setLocalPath(localStorage.getItem(storageKey) ?? "");
  }, [storageKey]);

  const chooseServer = async () => {
    if (!verifiedRootSession || currentResult?.active) return;
    setBusy("choose");
    try {
      const selected = await chooseBinaryFile(`选择 iOS Frida Server ${selectedVersion} · ${device.architecture ?? "arm64"}`);
      if (selected) setLocalPath(selected);
    } finally {
      setBusy(undefined);
    }
  };

  const start = async () => {
    if (!session || !verifiedRootSession || busy || currentResult?.active) return;
    const parsedDevicePort = validPort(devicePort);
    const parsedHostPort = validPort(hostPort, true);
    if (parsedDevicePort === null || parsedHostPort === null) {
      notify("warning", "请输入有效端口", "端口需在 1–65535 之间；本机端口留空时会自动选择。");
      return;
    }
    if (!localPath) {
      notify("warning", "请先选择 Server 文件");
      return;
    }
    if (profile === "custom" && !customVersion.trim()) {
      notify("warning", "请填写自定义版本标识");
      return;
    }
    setBusy("start");
    record("开始启动 iOS Frida Server", `${device.name} · ${selectedVersion}`, "info");
    try {
      const uploaded = await api.uploadIosFridaServer(session.sessionId, localPath);
      if (!uploaded.success) throw new Error(uploaded.message);
      onResultChange(uploaded);
      localStorage.setItem(storageKey, localPath);
      const started = await api.startIosFridaServer(session.sessionId, parsedDevicePort ?? undefined, parsedHostPort ?? undefined);
      if (!started.success) throw new Error(started.message);
      onResultChange(started);
      const endpoint = `${started.listenAddress ?? "127.0.0.1"}:${started.hostPort ?? "—"}`;
      notify("success", "iOS Frida Server 已就绪", `PID ${started.pid ?? "—"} · ${endpoint}`);
      record("启动 iOS Frida Server", `${device.name} · ${endpoint} → 设备 ${started.devicePort ?? parsedDevicePort}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "iOS Frida Server 启动失败", message);
      record("iOS Frida Server 启动失败", `${device.name} · ${message}`, "error");
    } finally {
      setBusy(undefined);
    }
  };

  const stop = async () => {
    if (!session || busy || !currentResult) return;
    setBusy("stop");
    try {
      const stopped = await api.stopIosFridaServer(session.sessionId);
      if (!stopped.success) throw new Error(stopped.message);
      onResultChange(undefined);
      notify("success", "iOS Frida 会话资源已清理", "Server、中性上传文件与本机隧道均已停止。");
      record("停止 iOS Frida Server", `${device.name} · ${stopped.remotePath}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "无法停止 iOS Frida Server", message);
      record("停止 iOS Frida Server 失败", `${device.name} · ${message}`, "error");
    } finally {
      setBusy(undefined);
    }
  };

  const copyEndpoint = async () => {
    if (!currentResult?.hostPort) return;
    const endpoint = `${currentResult.listenAddress ?? "127.0.0.1"}:${currentResult.hostPort}`;
    try {
      await navigator.clipboard.writeText(endpoint);
      notify("success", "本机连接地址已复制", endpoint);
    } catch {
      notify("warning", "无法写入剪贴板", endpoint);
    }
  };

  return <div className="debug-grid ios-frida-workspace">
    <Panel className="span-7" title={<><KeyRound size={17} /> iOS Frida Server 管理</>}>
      <div className="frida-profile-hero"><span className="quick-icon quick-purple"><KeyRound size={23} /></span><div><h2>越狱 iOS · SSH 会话托管</h2><p>选择你自己的 Mach-O Server，Mobius 以中性文件名上传，并自动建立仅本机可访问的 SSH 隧道。</p></div></div>
      {!verifiedRootSession ? <div className="ios-frida-connect-required">
        <InlineNotice tone="warning" title={session?.connected ? "SSH 会话不是 Root" : "先连接并验证 SSH"}>{session?.connected ? `当前会话 UID ${session.remoteUid ?? "未知"}；启动 Server 需要经验证的 root（UID 0）会话。` : "进入“文件”页会使用默认实验机账号一键连接，也可在设置中切换私钥；USB 自动配合 iproxy。"}</InlineNotice>
        <Button variant="primary" icon={<Link2 size={14} />} onClick={onOpenFiles}>{session?.connected ? "去文件页修改 SSH 设置" : "去文件页连接 SSH"}</Button>
      </div> : <div className="ios-frida-config form-stack">
        <InlineNotice tone="success" title="Root SSH 已验证">{session.mode === "usb" ? `SSH + iproxy 已连接本机 ${session.sshHost}:${session.sshPort}` : `局域网 SSH 已连接 ${session.sshHost}:${session.sshPort}`}，操作仅绑定此会话。</InlineNotice>
        <div className="frida-version-picker"><button className={profile === "16.1.4" ? "active" : ""} disabled={!!currentResult?.active || !!busy} onClick={() => setProfile("16.1.4")}><strong>16.1.4</strong><small>默认兼容槽</small></button><button className={profile === "17.17.0" ? "active" : ""} disabled={!!currentResult?.active || !!busy} onClick={() => setProfile("17.17.0")}><strong>17.17.0</strong><small>最新稳定槽</small></button><button className={profile === "custom" ? "active" : ""} disabled={!!currentResult?.active || !!busy} onClick={() => setProfile("custom")}><strong>自定义</strong><small>其他版本</small></button></div>
        {profile === "custom" && <label className="field"><span className="field-label">版本标识</span><input value={customVersion} disabled={!!currentResult?.active || !!busy} onChange={(event) => setCustomVersion(event.target.value.replace(/[^0-9A-Za-z._-]/g, "").slice(0, 40))} placeholder="例如 17.16.2" /></label>}
        <label className="field"><span className="field-label">Server 文件 · {selectedVersion}</span><div className="path-input"><input readOnly value={localPath} placeholder="从本机选择 Mach-O Server" title={localPath} /><button type="button" disabled={!!currentResult?.active || !!busy} onClick={() => void chooseServer()} title="选择文件">{busy === "choose" ? <LoaderCircle className="spin" size={14} /> : <FileUp size={14} />}</button></div><span className="field-hint">Mobius 不下载、不绑定 Server；后端会验证这是普通文件且具有 Mach-O 文件头。</span></label>
        <div className="field-row"><label className="field"><span className="field-label">设备监听端口</span><input value={devicePort} disabled={!!currentResult?.active || !!busy} inputMode="numeric" onChange={(event) => setDevicePort(event.target.value.replace(/\D/g, "").slice(0, 5))} /></label><label className="field"><span className="field-label">本机转发端口</span><input value={hostPort} disabled={!!currentResult?.active || !!busy} inputMode="numeric" onChange={(event) => setHostPort(event.target.value.replace(/\D/g, "").slice(0, 5))} placeholder="留空自动选择" /></label></div>
        <div className="ios-frida-primary-actions"><Button icon={<FileUp size={14} />} disabled={!!currentResult?.active || !!busy} onClick={() => void chooseServer()}>选择 Server</Button><Button variant="primary" icon={busy === "start" ? <LoaderCircle className="spin" size={14} /> : <Play size={14} />} disabled={!localPath || !!currentResult?.active || !!busy || (profile === "custom" && !customVersion.trim())} onClick={() => void start()}>{busy === "start" ? "正在上传并启动…" : "上传、启动并自动转发"}</Button></div>
      </div>}
    </Panel>
    <Panel className="span-5" title="当前 SSH 会话资源" action={currentResult && <Button variant="ghost" icon={busy === "stop" ? <LoaderCircle className="spin" size={14} /> : <CircleStop size={14} />} disabled={!!busy || !session} onClick={() => void stop()}>{currentResult.active ? "停止并清理" : "清理上传文件"}</Button>}>
      {currentResult ? <div className="debug-facts ios-frida-runtime">
        <div><StatusDot status={currentResult.active ? "running" : "muted"} /><span><strong>{currentResult.active ? "Server 正在运行" : "Server 未运行"}</strong><small>{currentResult.message}</small></span></div>
        <div><Code2 size={14} /><span><strong>{currentResult.remotePath}</strong><small>设备端中性路径 · 停止时仅清理此会话创建的文件</small></span></div>
        {currentResult.active && <><div><Activity size={14} /><span><strong>PID {currentResult.pid ?? "—"}</strong><small>设备 127.0.0.1:{currentResult.devicePort ?? "—"}</small></span></div><div><Link2 size={14} /><span><strong>{currentResult.listenAddress ?? "127.0.0.1"}:{currentResult.hostPort ?? "—"}</strong><small>本机 SSH 转发 · 隧道 PID {currentResult.tunnelPid ?? "—"}</small></span><Button variant="ghost" icon={<Clipboard size={13} />} onClick={() => void copyEndpoint()}>复制</Button></div></>}
      </div> : <EmptyState icon={<KeyRound size={27} />} title={verifiedRootSession ? "尚未由 Mobius 启动" : "等待 Root SSH 会话"} detail={verifiedRootSession ? "选择一个 Server 文件后即可上传、启动并获得本机连接地址。" : "Mobius 只接受已验证的 Root SSH 会话；SSH 密码仅驻留当前运行内存。"} />}
      <div className="debug-policy"><Code2 size={17} /><div><strong>中性远端名称</strong><p>远端生成的完整路径不含工具关键字，界面仍显示真实路径、PID 和端口。</p></div></div>
      <div className="debug-policy"><KeyRound size={17} /><div><strong>精确归属与自动清理</strong><p>停止前核对精确路径；关闭 SSH 会话或应用时，只清理 Mobius 在此会话创建的资源。</p></div></div>
    </Panel>
  </div>;
}
