import { ArrowLeft, Cable, KeyRound, Link2, LoaderCircle, RadioTower, RefreshCw, Search, ShieldCheck, Smartphone, Wifi } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Field, InlineNotice, Panel, StatusBadge, StatusDot, Tabs } from "../components/Ui";
import { api } from "../lib/api";
import type { ActivityItem, Device, DevicePlatform, ScanResult, ToastMessage } from "../types";

type ConnectMode = "pair" | "manual" | "legacy";
type DiscoveryPhase = "idle" | "scanning" | "connecting" | "ready" | "empty" | "error";
type EndpointState = "connecting" | "connected" | "failed";

interface DeviceConnectProps {
  initialMode: ConnectMode;
  initialPlatform?: DevicePlatform;
  overlay?: boolean;
  defaultCidr: string;
  defaultPorts: string;
  onBack: () => void;
  onRefreshDevices: () => Promise<void>;
  onRegisterIosEndpoint: (device: Device) => void;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}

function privateIpv4(value: string) {
  const parts = value.trim().split(".");
  if (parts.length !== 4 || parts.some((part) => !/^\d{1,3}$/.test(part))) return undefined;
  const octets = parts.map(Number);
  if (octets.some((part) => part < 0 || part > 255)) return undefined;
  const [first, second] = octets;
  const allowed = first === 10
    || first === 127
    || (first === 172 && second >= 16 && second <= 31)
    || (first === 192 && second === 168);
  return allowed ? octets.join(".") : undefined;
}

function friendlyDiscoveryError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  if (/not an RFC1918|no_private_network|private network/i.test(message)) {
    return "没有识别到可扫描的私有局域网。请确认电脑和手机连接到同一 Wi-Fi；VPN 或虚拟网卡可能暂时遮蔽真实网卡，关闭后可直接重试。";
  }
  if (/cidr_not_local|active local subnet|Requested subnet/i.test(message)) {
    return "自定义网段与电脑当前所在局域网不一致。请清空自定义网段，使用自动识别后重试。";
  }
  if (/scan_already_running|already running/i.test(message)) {
    return "设备发现仍在进行中，请稍候再试。";
  }
  return message;
}

export default function DeviceConnect({ initialMode, initialPlatform = "android", overlay = false, defaultCidr, defaultPorts, onBack, onRefreshDevices, onRegisterIosEndpoint, notify, record }: DeviceConnectProps) {
  const [platform, setPlatform] = useState<DevicePlatform>(initialPlatform);
  const [mode, setMode] = useState<ConnectMode>(initialMode);
  const [address, setAddress] = useState("");
  const [pairCode, setPairCode] = useState("");
  const [cidr, setCidr] = useState(defaultCidr);
  const [ports, setPorts] = useState(defaultPorts);
  const [scanResults, setScanResults] = useState<ScanResult[]>([]);
  const [discoveryPhase, setDiscoveryPhase] = useState<DiscoveryPhase>("idle");
  const [discoveryError, setDiscoveryError] = useState("");
  const [formError, setFormError] = useState("");
  const [endpointStates, setEndpointStates] = useState<Record<string, EndpointState>>({});
  const [iosName, setIosName] = useState("iPhone SSH");
  const [iosHost, setIosHost] = useState("");
  const [iosPort, setIosPort] = useState("22");
  const [busy, setBusy] = useState(false);
  const autoDiscoveryStarted = useRef(false);
  const discoveryGeneration = useRef(0);
  const componentMounted = useRef(true);
  const [recentAddresses, setRecentAddresses] = useState<string[]>(() => {
    if (typeof localStorage === "undefined") return [];
    try {
      const parsed = JSON.parse(localStorage.getItem("mobius.android.recent-endpoints.v1") ?? "[]");
      return Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string").slice(0, 5) : [];
    } catch {
      return [];
    }
  });
  const parsedPorts = useMemo(() => ports.split(",").map((part) => Number(part.trim())).filter((port) => Number.isInteger(port) && port > 0 && port <= 65535), [ports]);

  const run = async (task: () => Promise<void>) => {
    setBusy(true);
    setFormError("");
    try { await task(); }
    catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setFormError(message);
      notify("error", "设备连接失败", message);
    }
    finally { setBusy(false); }
  };

  const connect = () => run(async () => {
    const input = address.trim();
    const target = mode === "manual" && input && !input.includes(":") ? `${input}:5555` : input;
    if (!target) throw new Error("请输入设备 IP 与端口");
    const result = mode === "pair" ? await api.pair(target, pairCode.trim()) : await api.connect(target);
    if (!result.success) throw new Error(result.message);
    if (mode === "manual") {
      const nextRecent = [target, ...recentAddresses.filter((item) => item !== target)].slice(0, 5);
      setRecentAddresses(nextRecent);
      if (typeof localStorage !== "undefined") localStorage.setItem("mobius.android.recent-endpoints.v1", JSON.stringify(nextRecent));
    }
    notify("success", mode === "pair" ? "无线调试已配对" : "ADB 连接成功", result.message);
    record(mode === "pair" ? "配对 Android 设备" : "连接 Android 设备", target);
    await onRefreshDevices();
    onBack();
  });

  const connectDiscovered = async (item: ScanResult, returnAfterConnect = true) => {
    const target = `${item.address}:${item.port}`;
    setEndpointStates((current) => ({ ...current, [target]: "connecting" }));
    try {
      const result = await api.connect(target);
      if (!result.success) throw new Error(result.message);
      setEndpointStates((current) => ({ ...current, [target]: "connected" }));
      record("自动连接 Android 设备", target);
      if (returnAfterConnect) {
        await onRefreshDevices();
        notify("success", "Android 设备已连接", target);
        onBack();
      }
      return true;
    } catch {
      setEndpointStates((current) => ({ ...current, [target]: "failed" }));
      return false;
    }
  };

  const retryDiscovered = async (item: ScanResult) => {
    if (busy) return;
    setBusy(true);
    setDiscoveryError("");
    const connected = await connectDiscovered(item);
    if (!connected) {
      setDiscoveryPhase("ready");
      setDiscoveryError(`无法连接 ${item.address}:${item.port}。请确认设备仍在线，并在手机端允许调试授权。`);
    }
    setBusy(false);
  };

  const scan = async (useCustomSubnet = false) => {
    if (busy) return;
    const generation = ++discoveryGeneration.current;
    const scanPorts = parsedPorts.length ? parsedPorts : [5555];
    if (useCustomSubnet && cidr && (!/^((10\.)|(192\.168\.)|(172\.(1[6-9]|2\d|3[01])\.))/.test(cidr) || !cidr.endsWith("/24"))) {
      setDiscoveryPhase("error");
      setDiscoveryError("自定义网段必须是 RFC1918 私有 /24，例如 192.168.100.0/24。");
      return;
    }
    setBusy(true);
    setDiscoveryPhase("scanning");
    setDiscoveryError("");
    setScanResults([]);
    setEndpointStates({});
    const selectedCidr = useCustomSubnet ? cidr.trim() || undefined : undefined;
    record("自动发现 Android 调试端点", `${selectedCidr || "自动识别电脑当前私有 /24"} · ${scanPorts.join(", ")}`, "info");
    try {
      const results = await api.scan(selectedCidr, scanPorts);
      if (!componentMounted.current || generation !== discoveryGeneration.current) return;
      setScanResults(results);
      if (!results.length) {
        setDiscoveryPhase("empty");
        notify("info", "未发现 Android 设备", "请确认手机与电脑处于同一局域网，并已开启 5555 网络调试。");
        return;
      }

      const adbEndpoints = results.filter((item) => item.state === "adb");
      if (!adbEndpoints.length) {
        setDiscoveryPhase("ready");
        notify("info", "发现可疑端口", "尚未确认到 ADB 服务，可在结果中手动尝试连接。");
        return;
      }

      setDiscoveryPhase("connecting");
      let connected = 0;
      for (const endpoint of adbEndpoints) {
        if (!componentMounted.current || generation !== discoveryGeneration.current) return;
        if (await connectDiscovered(endpoint, false)) connected += 1;
      }
      if (!componentMounted.current || generation !== discoveryGeneration.current) return;
      if (connected > 0) {
        notify("success", "局域网设备已自动连接", `成功连接 ${connected} 台 Android 设备。`);
        await onRefreshDevices();
        onBack();
        return;
      }
      setDiscoveryPhase("ready");
      setDiscoveryError("发现了 ADB 设备，但连接未成功。请确认手机端已允许这台电脑的调试授权，然后点击重试连接。");
    } catch (error) {
      if (!componentMounted.current || generation !== discoveryGeneration.current) return;
      const message = friendlyDiscoveryError(error);
      setDiscoveryPhase("error");
      setDiscoveryError(message);
      notify("error", "自动发现失败", message);
    } finally {
      if (componentMounted.current && generation === discoveryGeneration.current) setBusy(false);
    }
  };

  const changeMode = (next: ConnectMode) => {
    discoveryGeneration.current += 1;
    setBusy(false);
    setFormError("");
    if (next !== "legacy") {
      autoDiscoveryStarted.current = false;
      setDiscoveryPhase("idle");
    }
    setMode(next);
  };

  const changePlatform = (next: DevicePlatform) => {
    discoveryGeneration.current += 1;
    setBusy(false);
    setFormError("");
    if (next !== "android") {
      autoDiscoveryStarted.current = false;
      setDiscoveryPhase("idle");
    }
    setPlatform(next);
  };

  useEffect(() => {
    componentMounted.current = true;
    return () => { componentMounted.current = false; };
  }, []);

  useEffect(() => {
    if (platform !== "android" || mode !== "legacy" || autoDiscoveryStarted.current) return;
    autoDiscoveryStarted.current = true;
    void scan(false);
  }, [mode, platform]);

  const registerIos = () => {
    const host = privateIpv4(iosHost);
    const port = Number(iosPort);
    if (!host) {
      notify("warning", "请输入有效的私网 IP", "只接受 10/8、172.16/12、192.168/16 或 127/8 的 IPv4 地址。");
      return;
    }
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      notify("warning", "请输入有效 SSH 端口");
      return;
    }
    const endpoint = `${host}:${port}`;
    onRegisterIosEndpoint({
      id: `ios-ssh:${endpoint}`,
      name: iosName.trim() || `iPhone ${host}`,
      platform: "ios",
      osVersion: "待检测",
      state: "registered",
      transport: "wifi",
      address: endpoint,
      architecture: "arm64",
      connectionSource: "manual",
    });
    record("登记 iOS SSH 端点", `${iosName.trim() || "iPhone"} · ${endpoint}`, "info");
  };

  return <div className={`page device-connect-page ${overlay ? "device-connect-overlay" : ""}`} role={overlay ? "region" : undefined} aria-label={overlay ? "添加或连接设备" : undefined}>
    <div className="page-heading"><div><span className="eyebrow">DEVICE / ADD CONNECTION</span><h1>添加设备</h1><p>连接 Android 调试端点，或登记仅能通过局域网 SSH 访问的越狱 iOS 设备。</p></div><Button icon={<ArrowLeft size={15} />} onClick={onBack}>返回工作台</Button></div>
    <div className="connect-platform-picker"><Tabs value={platform} onChange={changePlatform} options={[{ id: "android", label: "Android / ADB" }, { id: "ios", label: "iOS / SSH" }]} /></div>
    {platform === "android" ? <>
    <Tabs value={mode} onChange={changeMode} options={[{ id: "legacy", label: "自动发现（默认）" }, { id: "pair", label: "无线配对" }, { id: "manual", label: "手动地址" }]} />
    <div className="network-grid">
      <Panel className="span-5" title={<>{mode === "legacy" ? <Search size={17} /> : mode === "pair" ? <ShieldCheck size={17} /> : <Cable size={17} />} {mode === "legacy" ? "扫描并自动连接" : mode === "pair" ? "Android 无线调试配对" : "连接已知端点"}</>}>
        {mode !== "legacy" ? <div className="form-stack">
          <InlineNotice tone="info" title={mode === "pair" ? "用于 Android 11+ 无线调试" : "仅在自动发现不可用时使用"}>{mode === "pair" ? "在开发者选项中打开“使用配对码配对设备”，输入屏幕当前显示的 IP、配对端口和六位码。" : "输入已知设备地址；只填 IP 会自动补充 5555 端口。"}</InlineNotice>
          <Field label={mode === "pair" ? "配对地址" : "设备地址"} hint={mode === "pair" ? "配对完成后，设备可能使用另一个连接端口；刷新列表即可发现。" : "只填 IP 时自动使用 5555；也可填 IP:端口。"}><input autoFocus value={address} onChange={(event) => setAddress(event.target.value)} placeholder={mode === "pair" ? "192.168.1.42:37123" : "192.168.1.42"} /></Field>
          {mode === "manual" && recentAddresses.length > 0 && <div className="recent-endpoints" aria-label="最近连接"><span>最近</span>{recentAddresses.map((endpoint) => <button type="button" key={endpoint} onClick={() => setAddress(endpoint)}>{endpoint}</button>)}</div>}
          {mode === "pair" && <Field label="六位配对码"><input value={pairCode} onChange={(event) => setPairCode(event.target.value.replace(/\D/g, "").slice(0, 6))} placeholder="123456" inputMode="numeric" autoComplete="one-time-code" /></Field>}
          {formError && <InlineNotice tone="danger" title="连接没有完成">{formError}</InlineNotice>}
          <Button variant="primary" icon={busy ? <LoaderCircle className="spin" size={15} /> : <Link2 size={15} />} disabled={busy || !address.trim() || (mode === "pair" && pairCode.length !== 6)} onClick={connect}>{mode === "pair" ? "配对并刷新" : "连接并刷新"}</Button>
        </div> : <div className="auto-discovery">
          <div className={`auto-discovery-hero phase-${discoveryPhase}`}>
            <div className="radar">{busy ? <LoaderCircle className="spin" size={24} /> : <Wifi size={24} />}<span /></div>
            <div>
              <strong>{discoveryPhase === "scanning" ? "正在识别当前局域网…" : discoveryPhase === "connecting" ? "已发现设备，正在自动连接…" : discoveryPhase === "empty" ? "当前网段没有发现设备" : discoveryPhase === "error" ? "自动发现暂时不可用" : scanResults.length ? `发现 ${scanResults.length} 个候选端点` : "无需输入 IP"}</strong>
              <small>{discoveryPhase === "scanning" ? "自动选择电脑所在的 192.168.x.x、10.x.x.x 或 172.16–31.x.x 网段，并快速探测 5555。" : "手机与电脑处于同一局域网且已开启 5555 时，Mobius 会发现并直接连接。"}</small>
            </div>
          </div>
          {discoveryError && <InlineNotice tone="danger" title={discoveryPhase === "error" ? "无法识别当前局域网" : "设备连接未完成"}>{discoveryError}</InlineNotice>}
          <Button className="discovery-primary" variant="primary" icon={busy ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />} disabled={busy} onClick={() => void scan(false)}>{busy ? discoveryPhase === "connecting" ? "正在自动连接…" : "正在扫描 5555…" : discoveryPhase === "idle" ? "立即扫描并连接" : "重新扫描并连接"}</Button>
          <details className="discovery-advanced">
            <summary>高级扫描设置</summary>
            <div>
              <div className="field-row"><Field label="指定私有 /24 网段" hint="留空时仍自动识别"><input value={cidr} onChange={(event) => setCidr(event.target.value)} placeholder="192.168.100.0/24" /></Field><Field label="端口"><input value={ports} onChange={(event) => setPorts(event.target.value.replace(/[^\d,]/g, ""))} placeholder="5555" /></Field></div>
              <Button variant="secondary" icon={<Search size={14} />} disabled={busy || !parsedPorts.length} onClick={() => void scan(true)}>按指定范围扫描</Button>
            </div>
          </details>
        </div>}
      </Panel>
      <Panel className="span-7" title={mode === "legacy" ? "自动发现结果" : "连接路径"} action={mode === "legacy" ? <span className="panel-summary">{discoveryPhase === "scanning" ? "扫描中" : discoveryPhase === "connecting" ? "连接中" : scanResults.length ? `${scanResults.length} 个候选` : "当前私网 · 5555"}</span> : undefined}>
        {mode === "legacy" ? scanResults.length ? <div className="scan-list"><div className="scan-head"><span>端点</span><span>识别状态</span><span>响应</span><span /></div>{scanResults.map((item) => {
          const target = `${item.address}:${item.port}`;
          const endpointState = endpointStates[target];
          return <div className="scan-row" key={target}><span><StatusDot status={endpointState === "connecting" ? "running" : endpointState === "failed" ? "error" : endpointState === "connected" || item.state === "adb" ? "success" : "warning"} /><code>{target}</code></span><span><StatusBadge tone={endpointState === "failed" ? "danger" : endpointState === "connected" || item.state === "adb" ? "success" : "warning"}>{endpointState === "connecting" ? "正在连接" : endpointState === "connected" ? "已连接" : endpointState === "failed" ? "连接失败" : item.state === "adb" ? "ADB 已确认" : "端口已开放"}</StatusBadge></span><span>{item.latencyMs} ms</span><Button variant="ghost" icon={endpointState === "connecting" ? <LoaderCircle className="spin" size={14} /> : <Link2 size={14} />} disabled={busy || endpointState === "connected"} onClick={() => void retryDiscovered(item)}>{endpointState === "connecting" ? "连接中" : endpointState === "connected" ? "已连接" : endpointState === "failed" ? "重试" : item.state === "adb" ? "连接" : "尝试"}</Button></div>;
        })}</div> : <div className="discovery-placeholder"><div className="radar">{discoveryPhase === "scanning" || discoveryPhase === "connecting" ? <LoaderCircle className="spin" size={23} /> : <Search size={23} />}<span /></div><h3>{discoveryPhase === "scanning" ? "正在扫描当前电脑所在的私有网段" : discoveryPhase === "connecting" ? "正在建立 ADB 连接" : discoveryPhase === "empty" ? "没有发现开启 5555 的 Android 设备" : discoveryPhase === "error" ? "检查网络后即可重试" : "打开页面后自动开始发现"}</h3><p>{discoveryPhase === "empty" ? "确认手机与电脑在同一 Wi-Fi，并已通过 adb tcpip 5555 开启网络调试。" : "不需要填写电脑 IP、网段或设备地址。"}</p></div> : <div className="connection-guide">
          <div><span>1</span><Smartphone size={19} /><div><strong>准备设备</strong><small>启用开发者选项和 USB / 无线调试</small></div></div>
          <div><span>2</span><Wifi size={19} /><div><strong>{mode === "pair" ? "读取一次性配对信息" : "确认地址"}</strong><small>输入设备屏幕显示的信息，不猜测目标</small></div></div>
          <div><span>3</span><ShieldCheck size={19} /><div><strong>确认授权</strong><small>设备端接受调试指纹后才进入在线状态</small></div></div>
        </div>}
      </Panel>
    </div>
    </> : <div className="network-grid">
      <Panel className="span-5" title={<><RadioTower size={17} /> 登记局域网 SSH 端点</>}>
        <div className="form-stack">
          <InlineNotice tone="info" title="适用于未被 USB 自动识别的设备">这里只保存显示名称、私网 IP 和端口。登记后进入“文件”页会自动尝试默认 root/alpine，也可改用私钥。</InlineNotice>
          <Field label="设备显示名称"><input autoFocus value={iosName} onChange={(event) => setIosName(event.target.value)} placeholder="测试 iPhone" maxLength={80} /></Field>
          <div className="field-row">
            <Field label="设备私网 IP" hint="仅接受明确的私网或本机回环 IPv4。"><input value={iosHost} onChange={(event) => setIosHost(event.target.value.replace(/[^\d.]/g, "").slice(0, 15))} placeholder="192.168.1.42" inputMode="decimal" /></Field>
            <Field label="SSH 端口"><input value={iosPort} onChange={(event) => setIosPort(event.target.value.replace(/\D/g, "").slice(0, 5))} placeholder="22" inputMode="numeric" /></Field>
          </div>
          <Button variant="primary" icon={<Link2 size={15} />} disabled={!iosHost.trim() || !iosPort} onClick={registerIos}>登记并连接文件</Button>
        </div>
      </Panel>
      <Panel className="span-7" title="下一步：安全认证">
        <div className="connection-guide">
          <div><span>1</span><Smartphone size={19} /><div><strong>登记明确端点</strong><small>Mobius 不扫描 SSH，也不会尝试其他主机</small></div></div>
          <div><span>2</span><KeyRound size={19} /><div><strong>自动尝试默认实验机账号</strong><small>账号密码只进入当前运行内存，不写设备登记</small></div></div>
          <div><span>3</span><ShieldCheck size={19} /><div><strong>连接后确认环境</strong><small>成功后直接显示文件，也可改成私钥认证</small></div></div>
        </div>
        <InlineNotice tone="warning" title="默认凭据只驻留内存">进入文件页后会自动尝试当前自用实验机默认账号；密码不进入命令行、日志或本地设置，连接失败会直接展开修改入口。</InlineNotice>
      </Panel>
    </div>}
  </div>;
}
