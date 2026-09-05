import { AlertTriangle, ArrowDownToLine, ArrowRight, ArrowUpFromLine, Cable, CheckCircle2, CircleDot, Clock3, Globe2, KeyRound, LoaderCircle, Network as NetworkIcon, Plus, RefreshCw, Router, Trash2, Unplug, Usb } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api } from "../lib/api";
import { Button, Field, InlineNotice, Panel, StatusBadge, StatusDot, Tabs } from "../components/Ui";
import type { ActivityItem, CreateIosPortTunnelRequest, Device, IosPortTunnel, IosPortTunnelDirection, IosPortTunnelTransport, IosSshSession, PortMapping, ToastMessage, ToolHealth } from "../types";

interface NetworkProps {
  activeDevice?: Device;
  initialTab?: "mapping" | "proxy";
  defaultProxyHost?: string;
  defaultProxyPort?: string;
  iosSession?: IosSshSession;
  tools?: ToolHealth[];
  onOpenIosFiles?: () => void;
  onOpenToolSettings?: () => void;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}

const networkPageCache: {
  tab?: "mapping" | "proxy";
  lastIntent?: "mapping" | "proxy";
  mappingDirection: "forward" | "reverse";
  localPort: string;
  remotePort: string;
  proxyHost?: string;
  proxyPort?: string;
  proxyMode?: "reverse" | "lan";
  mappingsByDevice: Map<string, PortMapping[]>;
} = {
  mappingDirection: "forward",
  localPort: "8080",
  remotePort: "8080",
  mappingsByDevice: new Map(),
};

export default function NetworkPage(props: NetworkProps) {
  if (props.activeDevice?.platform === "ios") return <IosNetworkWorkspace {...props} device={props.activeDevice} />;
  return <AndroidNetworkWorkspace {...props} />;
}

function AndroidNetworkWorkspace({ activeDevice, initialTab = "proxy", defaultProxyHost = "127.0.0.1", defaultProxyPort = "8080", notify, record }: NetworkProps) {
  const explicitTabChanged = networkPageCache.lastIntent !== undefined && networkPageCache.lastIntent !== initialTab;
  const [tab, setTabState] = useState<"mapping" | "proxy">(() => explicitTabChanged ? initialTab : networkPageCache.tab ?? initialTab);
  const [busyDeviceId, setBusyDeviceId] = useState<string>();
  const [mappings, setMappings] = useState<PortMapping[]>(() => activeDevice ? networkPageCache.mappingsByDevice.get(activeDevice.id) ?? [] : []);
  const [mappingsDeviceId, setMappingsDeviceId] = useState(activeDevice?.id ?? "");
  const [mappingDirection, setMappingDirectionState] = useState<"forward" | "reverse">(networkPageCache.mappingDirection);
  const [localPort, setLocalPortState] = useState(networkPageCache.localPort);
  const [remotePort, setRemotePortState] = useState(networkPageCache.remotePort);
  const [proxyHost, setProxyHostState] = useState(networkPageCache.proxyHost ?? (defaultProxyHost === "127.0.0.1" ? "" : defaultProxyHost));
  const [proxyPort, setProxyPortState] = useState(networkPageCache.proxyPort ?? defaultProxyPort);
  const [proxyMode, setProxyModeState] = useState<"reverse" | "lan">(networkPageCache.proxyMode ?? (defaultProxyHost === "127.0.0.1" ? "reverse" : "lan"));
  const androidReady = activeDevice?.platform === "android" && activeDevice.state === "online";
  const activeDeviceIdRef = useRef(activeDevice?.id);
  const mappingRequestNumbers = useRef(new Map<string, number>());
  activeDeviceIdRef.current = activeDevice?.id;
  const busy = busyDeviceId === activeDevice?.id;
  const visibleMappings = mappingsDeviceId === activeDevice?.id ? mappings : [];
  const setTab = (next: "mapping" | "proxy") => { networkPageCache.tab = next; setTabState(next); };
  const setMappingDirection = (next: "forward" | "reverse") => { networkPageCache.mappingDirection = next; setMappingDirectionState(next); };
  const setLocalPort = (next: string) => { networkPageCache.localPort = next; setLocalPortState(next); };
  const setRemotePort = (next: string) => { networkPageCache.remotePort = next; setRemotePortState(next); };
  const setProxyHost = (next: string) => { networkPageCache.proxyHost = next; setProxyHostState(next); };
  const setProxyPort = (next: string) => { networkPageCache.proxyPort = next; setProxyPortState(next); };
  const setProxyMode = (next: "reverse" | "lan") => { networkPageCache.proxyMode = next; setProxyModeState(next); };

  useEffect(() => {
    networkPageCache.lastIntent = initialTab;
    networkPageCache.tab = tab;
  }, [initialTab, tab]);

  const refreshMappings = async () => {
    const requestDevice = activeDevice;
    if (!androidReady || !requestDevice) {
      setMappingsDeviceId(requestDevice?.id ?? "");
      setMappings([]);
      return;
    }
    const requestDeviceId = requestDevice.id;
    const requestNumber = (mappingRequestNumbers.current.get(requestDeviceId) ?? 0) + 1;
    mappingRequestNumbers.current.set(requestDeviceId, requestNumber);
    const cached = networkPageCache.mappingsByDevice.get(requestDeviceId);
    if (activeDeviceIdRef.current === requestDeviceId) {
      setMappingsDeviceId(requestDeviceId);
      setMappings(cached ?? []);
    }
    try {
      const current = await api.mappings(requestDeviceId);
      networkPageCache.mappingsByDevice.set(requestDeviceId, current);
      if (activeDeviceIdRef.current !== requestDeviceId || mappingRequestNumbers.current.get(requestDeviceId) !== requestNumber) return;
      setMappingsDeviceId(requestDeviceId);
      setMappings(current);
    } catch {
      if (activeDeviceIdRef.current === requestDeviceId && mappingRequestNumbers.current.get(requestDeviceId) === requestNumber && !cached) {
        setMappingsDeviceId(requestDeviceId);
        setMappings([]);
      }
    }
  };

  useEffect(() => { void refreshMappings(); }, [activeDevice?.id, activeDevice?.platform, activeDevice?.state]);

  const run = async (task: () => Promise<void>) => {
    const operationDeviceId = activeDevice?.id ?? "";
    setBusyDeviceId(operationDeviceId);
    try { await task(); }
    catch (error) { notify("error", "网络操作失败", error instanceof Error ? error.message : String(error)); }
    finally { setBusyDeviceId((current) => current === operationDeviceId ? undefined : current); }
  };

  const createMapping = () => run(async () => {
    if (!activeDevice || !androidReady) throw new Error("请选择一台在线 Android 设备");
    const localNumber = Number(localPort);
    const remoteNumber = Number(remotePort);
    if (!localNumber || localNumber > 65535 || !remoteNumber || remoteNumber > 65535) throw new Error("请输入有效端口");
    const mapping: PortMapping = { serial: activeDevice.id, direction: mappingDirection, local: `tcp:${localNumber}`, remote: `tcp:${remoteNumber}` };
    const current = await api.mappings(activeDevice.id).catch(() => networkPageCache.mappingsByDevice.get(activeDevice.id) ?? []);
    const alreadyMapped = current.some((item) => item.direction === mapping.direction && item.local === mapping.local && item.remote === mapping.remote);
    if (alreadyMapped) {
      notify("info", "映射已在使用", `${mappingDirection.toUpperCase()} · ${mapping.local} → ${mapping.remote}`);
      networkPageCache.mappingsByDevice.set(activeDevice.id, current);
      if (activeDeviceIdRef.current === activeDevice.id) {
        setMappingsDeviceId(activeDevice.id);
        setMappings(current);
      }
      return;
    }
    const result = await api.createMapping(mapping);
    if (!result.success) throw new Error(result.message);
    notify("success", "端口映射已创建", `${mappingDirection.toUpperCase()} · ${mapping.local} → ${mapping.remote}`);
    record("创建端口映射", `${activeDevice.name} · ${mapping.local} → ${mapping.remote}`);
    await refreshMappings();
  });

  const removeMapping = (mapping: PortMapping) => run(async () => {
    const result = await api.removeMapping(mapping);
    if (!result.success) throw new Error(result.message);
    notify("success", "端口映射已移除", `${mapping.local} → ${mapping.remote}`);
    await refreshMappings();
  });

  const applyProxy = (setSystemProxy: boolean) => run(async () => {
    if (!activeDevice || !androidReady) throw new Error("请选择一台在线 Android 设备");
    const port = Number(proxyPort);
    if (!port || port > 65535) throw new Error("代理端口无效");
    if (proxyMode === "lan" && !proxyHost.trim()) throw new Error("请输入可被设备访问的测试主机地址");
    let reverseCreated = false;
    try {
      if (proxyMode === "reverse") {
        const endpoint = `tcp:${port}`;
        const existing = await api.mappings(activeDevice.id).catch(() => []);
        const alreadyMapped = existing.some((mapping) => mapping.direction === "reverse" && mapping.local === endpoint && mapping.remote === endpoint);
        if (!alreadyMapped) {
          const mapped = await api.createMapping({ serial: activeDevice.id, direction: "reverse", local: endpoint, remote: endpoint });
          if (!mapped.success) throw new Error(mapped.message);
          reverseCreated = true;
        }
      }
      if (proxyMode === "reverse" && !setSystemProxy) {
        notify("success", "USB Reverse 已创建", `设备 127.0.0.1:${port} → 本机 ${port}；未修改 Android 系统代理。`);
        record("创建代理 Reverse", `${activeDevice.name} · tcp:${port} · 未设置系统代理`);
        await refreshMappings();
        return;
      }
      const targetHost = proxyMode === "reverse" ? "127.0.0.1" : proxyHost.trim();
      const result = await api.setProxy(activeDevice.id, targetHost, port);
      if (!result.success) throw new Error(result.message);
      notify("success", "测试代理已就绪", proxyMode === "reverse" ? `设备 127.0.0.1:${port} → 本机 Burp ${port}` : `${targetHost}:${port}`);
      record("设置 Android 测试代理", `${activeDevice.name} · ${proxyMode === "reverse" ? "USB Reverse" : targetHost}:${port}`);
      await refreshMappings();
    } catch (error) {
      if (reverseCreated) await api.removeMapping({ serial: activeDevice.id, direction: "reverse", local: `tcp:${port}`, remote: `tcp:${port}` }).catch(() => undefined);
      throw error;
    }
  });

  const clearProxy = () => run(async () => {
    if (!activeDevice || !androidReady) throw new Error("请选择一台在线 Android 设备");
    const result = await api.clearProxy(activeDevice.id);
    if (!result.success) throw new Error(result.message);
    notify("success", "设备系统代理已恢复", "Reverse 映射保持不变，可继续供 Reqable 等工具使用。 ");
    record("恢复 Android 系统代理", activeDevice.name);
  });

  const removeProxyReverse = () => run(async () => {
    if (!activeDevice || !androidReady) throw new Error("请选择一台在线 Android 设备");
    const port = Number(proxyPort) || 8080;
    const endpoint = `tcp:${port}`;
    const current = await api.mappings(activeDevice.id).catch(() => networkPageCache.mappingsByDevice.get(activeDevice.id) ?? []);
    const existing = current.find((mapping) => mapping.direction === "reverse" && mapping.local === endpoint && mapping.remote === endpoint);
    if (!existing) {
      notify("info", "当前端口没有 Reverse", `${endpoint} 无需移除；系统代理未修改。`);
      return;
    }
    const result = await api.removeMapping(existing);
    if (!result.success) throw new Error(result.message);
    notify("success", "USB Reverse 已移除", `没有修改 Android 系统代理。tcp:${port}`);
    record("移除代理 Reverse", `${activeDevice.name} · tcp:${port}`);
    await refreshMappings();
  });

  const proxyEndpoint = `tcp:${Number(proxyPort) || 0}`;
  const matchingReverse = visibleMappings.find((mapping) => mapping.direction === "reverse" && mapping.local === proxyEndpoint && mapping.remote === proxyEndpoint);

  return <div className="page network-page">
    <div className="page-heading"><div><span className="eyebrow">TRAFFIC & PORTS</span><h1>网络</h1><p>测试代理与 ADB 端口映射；设备配对和发现已归入“设备”。</p></div></div>
    <Tabs value={tab} onChange={setTab} options={[{ id: "proxy", label: "测试代理" }, { id: "mapping", label: "端口映射" }]} />

    {tab === "proxy" && <div className="network-grid">
      <Panel title={<><Globe2 size={17} /> Android 测试代理</>} className="span-5">
        <div className="form-stack">
          <InlineNotice tone="info" title="隧道与系统代理分开控制">Reqable 等工具会自行接管代理时，只创建 Reverse；需要 Burp 全局代理时再选择“Reverse + 系统代理”。</InlineNotice>
          <Field label="目标设备"><div className="input-like">{androidReady ? activeDevice.name : "请选择在线 Android 设备"}</div></Field>
          <div className="proxy-mode-picker"><button className={proxyMode === "reverse" ? "active" : ""} onClick={() => setProxyMode("reverse")}><Cable size={18} /><span><strong>USB 反向隧道</strong><small>推荐 · 本机同端口</small></span></button><button className={proxyMode === "lan" ? "active" : ""} onClick={() => setProxyMode("lan")}><NetworkIcon size={18} /><span><strong>局域网直连</strong><small>设备访问主机 IP</small></span></button></div>
          <div className="field-row">{proxyMode === "lan" && <Field label="测试主机 IP"><input value={proxyHost} onChange={(event) => setProxyHost(event.target.value)} placeholder="192.168.1.2" /></Field>}<Field label="Burp 监听端口"><input value={proxyPort} onChange={(event) => setProxyPort(event.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></Field></div>
          <div className="preset-row"><span>常用</span><button onClick={() => setProxyPort("8080")}>Burp 8080</button><button onClick={() => setProxyPort("8081")}>Burp 8081</button><button onClick={() => setProxyPort("8888")}>Charles 8888</button></div>
          {proxyMode === "reverse" && <InlineNotice tone={matchingReverse ? "success" : "info"} title={matchingReverse ? "当前端口 Reverse 已活跃" : "当前端口尚未建立 Reverse"}>{matchingReverse ? `${proxyEndpoint} 已复用，再次点击不会重复创建。` : `当前设备共有 ${visibleMappings.length} 条映射。`} <button type="button" className="text-button" onClick={() => setTab("mapping")}>查看全部映射</button></InlineNotice>}
          <div className="button-row proxy-actions">{proxyMode === "reverse" ? <><Button variant="primary" icon={busy ? <LoaderCircle className="spin" size={14} /> : <Cable size={14} />} disabled={!androidReady || busy} onClick={() => applyProxy(false)}>仅创建 Reverse</Button><Button disabled={!androidReady || busy} onClick={() => applyProxy(true)}>Reverse + 系统代理</Button><Button variant="ghost" disabled={!androidReady || busy} onClick={removeProxyReverse}>移除 Reverse</Button></> : <Button variant="primary" icon={busy ? <LoaderCircle className="spin" size={14} /> : <Globe2 size={14} />} disabled={!androidReady || busy} onClick={() => applyProxy(true)}>设置系统代理</Button>}<Button variant="ghost" disabled={!androidReady || busy} onClick={clearProxy}>恢复系统代理</Button></div>
        </div>
      </Panel>
      <Panel title={<><Router size={17} /> 生效检查</>} className="span-7">
        <div className="proxy-checklist">
          <div><span className="check-icon"><CheckCircle2 size={18} /></span><div><strong>链路范围可见</strong><p>{proxyMode === "reverse" ? `设备 loopback:${proxyPort || "…"} 经 ADB 到主机同端口；是否写系统代理由你单独选择。` : "设备直接访问你填写的局域网地址。"}</p></div></div>
          <div><span className="pending-icon"><Clock3 size={18} /></span><div><strong>Burp 监听器</strong><p>应用前确认主机对应端口正在监听，且未暴露到不受信网络。</p></div></div>
          <div><span className="pending-icon"><CircleDot size={18} /></span><div><strong>HTTPS 信任</strong><p>系统证书、用户证书与应用证书固定是不同边界，代理成功不代表 TLS 一定可解密。</p></div></div>
        </div>
        <div className="security-footnote"><AlertTriangle size={15} /><span>只拦截你拥有或明确获准测试的应用流量。</span></div>
      </Panel>
    </div>}

    {tab === "mapping" && <div className="network-grid">
      <Panel title={<><Plus size={17} /> 新建映射</>} className="span-4">
        <div className="form-stack">
          <div className="direction-picker"><button className={mappingDirection === "forward" ? "active" : ""} onClick={() => setMappingDirection("forward")}><ArrowDownToLine /><strong>Forward</strong><small>主机 → 设备</small></button><button className={mappingDirection === "reverse" ? "active" : ""} onClick={() => setMappingDirection("reverse")}><ArrowUpFromLine /><strong>Reverse</strong><small>设备 → 主机</small></button></div>
          <Field label="目标设备"><div className="input-like">{androidReady ? activeDevice.name : "请选择在线 Android 设备"}</div></Field>
          <div className="mapping-fields"><Field label={mappingDirection === "forward" ? "主机端口" : "设备端口"}><div className="prefix-input"><span>tcp:</span><input value={localPort} onChange={(event) => setLocalPort(event.target.value.replace(/\D/g, "").slice(0, 5))} /></div></Field><ArrowRight size={16} /><Field label={mappingDirection === "forward" ? "设备端口" : "主机端口"}><div className="prefix-input"><span>tcp:</span><input value={remotePort} onChange={(event) => setRemotePort(event.target.value.replace(/\D/g, "").slice(0, 5))} /></div></Field></div>
          <div className="preset-row"><span>预设</span><button onClick={() => { setLocalPort("8080"); setRemotePort("8080"); }}>Burp</button><button onClick={() => { setLocalPort("9222"); setRemotePort("9222"); }}>WebView</button><button onClick={() => { setLocalPort("27042"); setRemotePort("27042"); }}>Frida</button></div>
          <Button variant="primary" disabled={!androidReady || busy} onClick={createMapping}>创建映射</Button>
        </div>
      </Panel>
      <Panel title="活跃映射" action={<button className="icon-button" aria-label="刷新映射" onClick={() => void refreshMappings()}><RefreshCw size={15} /></button>} className="span-8">
        {visibleMappings.length ? <div className="mapping-list"><div className="mapping-head"><span>方向</span><span>监听端</span><span>目标端</span><span>状态</span><span /></div>{visibleMappings.map((mapping, index) => <div className="mapping-row" key={mapping.id ?? `${mapping.direction}-${index}`}><span><StatusBadge tone="info">{mapping.direction.toUpperCase()}</StatusBadge></span><code>{mapping.local}</code><code>{mapping.remote}</code><span className="inline-icon"><StatusDot status="success" /> 活跃</span><button className="icon-button danger-icon" onClick={() => removeMapping(mapping)} title="移除"><Trash2 size={15} /></button></div>)}</div> : <div className="quiet-state large"><Unplug size={25} /><strong>没有活跃映射</strong><span>映射只会创建在当前明确选择的 Android 设备上。</span></div>}
      </Panel>
    </div>}
  </div>;
}

function IosNetworkWorkspace({ device, iosSession, tools = [], onOpenIosFiles, onOpenToolSettings, notify, record }: NetworkProps & { device: Device }) {
  const usbDevice = device.transport === "usb" || device.transport === "usbmux";
  const iproxyReady = tools.some((tool) => tool.id === "iproxy" && tool.state === "ready");
  const sshReady = !!iosSession?.connected && !!iosSession.jailbreakConfirmed;
  const [transport, setTransport] = useState<IosPortTunnelTransport>(usbDevice ? "iproxy" : "ssh");
  const [direction, setDirection] = useState<IosPortTunnelDirection>("hostToDevice");
  const [hostPort, setHostPort] = useState("8080");
  const [devicePort, setDevicePort] = useState("8080");
  const [tunnels, setTunnels] = useState<IosPortTunnel[]>([]);
  const [busy, setBusy] = useState<"refresh" | "create" | string>();
  const requestNumber = useRef(0);

  const refreshTunnels = async () => {
    const request = ++requestNumber.current;
    setBusy((current) => current ?? "refresh");
    try {
      const all = await api.listIosPortTunnels();
      if (request !== requestNumber.current) return;
      setTunnels(all.filter((tunnel) => tunnel.udid === device.id || (!!iosSession && tunnel.sessionId === iosSession.sessionId)));
    } catch (error) {
      if (request !== requestNumber.current) return;
      notify("error", "无法读取 iOS 隧道", error instanceof Error ? error.message : String(error));
    } finally {
      if (request === requestNumber.current) setBusy((current) => current === "refresh" ? undefined : current);
    }
  };

  useEffect(() => {
    setTransport(usbDevice ? "iproxy" : "ssh");
    setDirection("hostToDevice");
    setTunnels([]);
    void refreshTunnels();
  }, [device.id, usbDevice, iosSession?.sessionId]);

  const chooseTransport = (next: IosPortTunnelTransport) => {
    if (next === "iproxy") setDirection("hostToDevice");
    setTransport(next);
  };

  const chooseDirection = (next: IosPortTunnelDirection) => {
    if (next === "deviceToHost") setTransport("ssh");
    setDirection(next);
  };

  const applyPreset = (host: number, remote: number, nextTransport: IosPortTunnelTransport = transport) => {
    setHostPort(String(host));
    setDevicePort(String(remote));
    chooseTransport(nextTransport);
  };

  const createTunnel = async () => {
    if (busy) return;
    const parsedHostPort = Number(hostPort);
    const parsedDevicePort = Number(devicePort);
    if (!Number.isInteger(parsedHostPort) || parsedHostPort < 1 || parsedHostPort > 65535 || !Number.isInteger(parsedDevicePort) || parsedDevicePort < 1 || parsedDevicePort > 65535) {
      notify("warning", "端口无效", "本机端口和 iPhone 端口都需在 1–65535 之间。");
      return;
    }
    if (transport === "iproxy" && (!usbDevice || !iproxyReady)) {
      notify("warning", "USB 隧道不可用", !usbDevice ? "当前设备不是 USB/usbmux 连接。" : "请先在设置中配置 iproxy。");
      return;
    }
    if (transport === "ssh" && (!sshReady || !iosSession)) {
      notify("warning", "请先连接 iOS SSH", "SSH 隧道会复用当前设备的认证会话。");
      onOpenIosFiles?.();
      return;
    }
    const request: CreateIosPortTunnelRequest = {
      udid: device.id,
      sessionId: transport === "ssh" ? iosSession?.sessionId : undefined,
      transport,
      direction,
      hostPort: parsedHostPort,
      devicePort: parsedDevicePort,
    };
    setBusy("create");
    try {
      const created = await api.createIosPortTunnel(request);
      setTunnels((current) => [...current.filter((item) => item.tunnelId !== created.tunnelId), created]);
      const path = direction === "hostToDevice"
        ? `本机 127.0.0.1:${created.hostPort} → iPhone:${created.devicePort}`
        : `iPhone 127.0.0.1:${created.devicePort} → 本机:${created.hostPort}`;
      notify("success", "iOS 端口隧道已创建", path);
      record("创建 iOS 端口隧道", `${device.name} · ${created.transport.toUpperCase()} · ${path}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "iOS 隧道创建失败", message);
      record("iOS 隧道创建失败", `${device.name} · ${message}`, "error");
    } finally {
      setBusy(undefined);
    }
  };

  const removeTunnel = async (tunnel: IosPortTunnel) => {
    if (busy) return;
    setBusy(tunnel.tunnelId);
    try {
      const result = await api.removeIosPortTunnel(tunnel.tunnelId);
      if (!result.success) throw new Error(result.message);
      setTunnels((current) => current.filter((item) => item.tunnelId !== tunnel.tunnelId));
      notify("success", "iOS 端口隧道已停止", `${tunnel.bindAddress}:${tunnel.direction === "hostToDevice" ? tunnel.hostPort : tunnel.devicePort}`);
      record("停止 iOS 端口隧道", `${device.name} · PID ${tunnel.pid}`);
    } catch (error) {
      notify("error", "无法停止 iOS 隧道", error instanceof Error ? error.message : String(error));
    } finally {
      setBusy(undefined);
    }
  };

  const transportReady = transport === "iproxy" ? usbDevice && iproxyReady : sshReady;
  const preview = direction === "hostToDevice"
    ? `本机 127.0.0.1:${hostPort || "…"}  →  iPhone 127.0.0.1:${devicePort || "…"}`
    : `iPhone 127.0.0.1:${devicePort || "…"}  →  本机 127.0.0.1:${hostPort || "…"}`;

  return <div className="page network-page ios-network-page">
    <div className="page-heading"><div><span className="eyebrow">IOS PORT TUNNELS</span><h1>iOS 网络</h1><p>封装 iproxy 与 SSH 隧道；所有监听默认只绑定本机回环地址。</p></div><Button icon={<RefreshCw className={busy === "refresh" ? "spin" : ""} size={14} />} disabled={!!busy} onClick={() => void refreshTunnels()}>刷新隧道</Button></div>
    <div className="ios-network-session-strip">
      <span><StatusDot status={usbDevice && iproxyReady ? "success" : "muted"} /><strong>USB / iproxy</strong><small>{usbDevice ? iproxyReady ? "可直接创建本机到 iPhone 的映射" : "工具未就绪" : "当前不是 USB 连接"}</small></span>
      <span><StatusDot status={sshReady ? "success" : "warning"} /><strong>SSH 隧道</strong><small>{sshReady && iosSession ? `${iosSession.mode === "usb" ? "USB SSH" : "局域网 SSH"} · ${iosSession.username}@${iosSession.sshHost}:${iosSession.sshPort}` : "连接后支持双向转发"}</small></span>
      {!sshReady && <Button variant="ghost" icon={<KeyRound size={14} />} onClick={onOpenIosFiles}>连接 SSH</Button>}
    </div>
    <div className="network-grid">
      <Panel className="span-4" title={<><Plus size={17} /> 新建 iOS 隧道</>}>
        <div className="form-stack ios-tunnel-form">
          <div className="ios-tunnel-transport">
            <button className={transport === "iproxy" ? "active" : ""} disabled={!usbDevice} onClick={() => chooseTransport("iproxy")}><Usb size={18} /><span><strong>USB / iproxy</strong><small>无需 SSH · 本机访问设备</small></span></button>
            <button className={transport === "ssh" ? "active" : ""} onClick={() => chooseTransport("ssh")}><KeyRound size={18} /><span><strong>SSH 隧道</strong><small>支持两个方向</small></span></button>
          </div>
          <div className="direction-picker"><button className={direction === "hostToDevice" ? "active" : ""} onClick={() => chooseDirection("hostToDevice")}><ArrowDownToLine /><strong>本机访问 iPhone</strong><small>PC → 设备服务</small></button><button className={direction === "deviceToHost" ? "active" : ""} onClick={() => chooseDirection("deviceToHost")}><ArrowUpFromLine /><strong>iPhone 访问本机</strong><small>设备 → PC 服务</small></button></div>
          <div className="mapping-fields"><Field label="本机端口"><div className="prefix-input"><span>tcp:</span><input value={hostPort} onChange={(event) => setHostPort(event.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></div></Field><ArrowRight size={16} /><Field label="iPhone 端口"><div className="prefix-input"><span>tcp:</span><input value={devicePort} onChange={(event) => setDevicePort(event.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></div></Field></div>
          <div className="preset-row"><span>预设</span><button onClick={() => applyPreset(2222, 22, "iproxy")}>SSH</button><button onClick={() => applyPreset(27042, 27042)}>Frida</button><button onClick={() => applyPreset(8080, 8080)}>HTTP 8080</button><button onClick={() => applyPreset(1234, 1234)}>调试端口</button></div>
          <div className="ios-tunnel-preview"><StatusDot status={transportReady ? "success" : "warning"} /><code>{preview}</code><small>{transport === "iproxy" ? "iproxy 仅支持本机访问 iPhone" : direction === "hostToDevice" ? "SSH -L · 本地转发" : "SSH -R · 远程转发"}</small></div>
          {!iproxyReady && transport === "iproxy" && <InlineNotice tone="warning" title="需要 iproxy">安装 libusbmuxd 工具或在设置中指定 iOS 工具目录。 <button className="text-button" onClick={onOpenToolSettings}>打开工具链设置</button></InlineNotice>}
          {!sshReady && transport === "ssh" && <InlineNotice tone="warning" title="需要已验证的 SSH 会话">连接一次后即可复用当前账号和认证，不需要再次输入命令。 <button className="text-button" onClick={onOpenIosFiles}>连接 SSH</button></InlineNotice>}
          <Button variant="primary" icon={busy === "create" ? <LoaderCircle className="spin" size={14} /> : transport === "iproxy" ? <Usb size={14} /> : <KeyRound size={14} />} disabled={!transportReady || !!busy} onClick={() => void createTunnel()}>创建隧道</Button>
        </div>
      </Panel>
      <Panel className="span-8" title="Mobius 管理的活跃隧道" action={<StatusBadge tone={tunnels.length ? "success" : "neutral"}>{tunnels.length} 条</StatusBadge>}>
        {tunnels.length ? <div className="mapping-list ios-tunnel-list"><div className="mapping-head"><span>通道</span><span>监听端</span><span>目标端</span><span>状态</span><span /></div>{tunnels.map((tunnel) => {
          const toDevice = tunnel.direction === "hostToDevice";
          return <div className="mapping-row" key={tunnel.tunnelId}><span><StatusBadge tone={tunnel.transport === "iproxy" ? "info" : "purple"}>{tunnel.transport === "iproxy" ? "IPROXY" : toDevice ? "SSH -L" : "SSH -R"}</StatusBadge></span><code>{toDevice ? `${tunnel.bindAddress}:${tunnel.hostPort}` : `iPhone:${tunnel.devicePort}`}</code><code>{toDevice ? `iPhone:${tunnel.devicePort}` : `${tunnel.bindAddress}:${tunnel.hostPort}`}</code><span className="inline-icon"><StatusDot status={tunnel.active ? "success" : "warning"} />{tunnel.active ? "活跃" : "已退出"}</span><button className="icon-button danger-icon" disabled={!!busy} onClick={() => void removeTunnel(tunnel)} title="停止并移除"><Trash2 size={15} /></button></div>;
        })}</div> : <div className="quiet-state large"><Unplug size={25} /><strong>没有由 Mobius 创建的隧道</strong><span>创建后可在这里查看方向、端口、进程和状态；退出应用时会自动清理。</span></div>}
        <div className="ios-tunnel-safety"><AlertTriangle size={14} /><span>仅管理本工具创建的进程；不会关闭其他 iproxy 或 SSH 会话，也不会自动修改 iOS 系统代理。</span></div>
      </Panel>
    </div>
  </div>;
}
