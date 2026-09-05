import { AppWindow, ArchiveRestore, Box, ChevronDown, Copy, Eraser, FileArchive, FileKey2, Fingerprint, FolderDown, Info, LoaderCircle, PackageCheck, PackageOpen, Play, RefreshCw, Search, ShieldAlert, ShieldCheck, Smartphone, Square, Trash2, Upload } from "lucide-react";
import { DragEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Button, EmptyState, InlineNotice, Modal, Panel, StatusBadge, Tabs } from "../components/Ui";
import { chooseDirectory, choosePackageFile, packageSelectionFromFile } from "../lib/dialog";
import type { SelectedPackageFile } from "../lib/dialog";
import { api, runningInDesktop } from "../lib/api";
import { formatBytes } from "../lib/format";
import type { ActivityItem, Device, InstalledApp, IosAppCapabilities, IosPackageInstallerId, IosSshSession, PackageAnalysis, ToastMessage } from "../types";

type AppsView = "package" | "installed";

const appsPageCache: {
  view?: AppsView;
  lastIntent?: AppsView;
  analysis?: PackageAnalysis;
  appSearch: string;
  installedByDevice: Map<string, InstalledApp[]>;
} = { appSearch: "", installedByDevice: new Map() };

interface AppsProps {
  activeDevice?: Device;
  iosSshReady?: boolean;
  iosSession?: IosSshSession;
  defaultExportDirectory?: string;
  initialView?: AppsView;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}

interface InstallPlan {
  deviceId: string;
  deviceName: string;
  platform: "android" | "ios";
  packagePath: string;
  fileName: string;
  appName: string;
  packageName: string;
  method: "android" | "usb" | "ssh";
  iosSessionId?: string;
  iosInstallerId?: IosPackageInstallerId;
}

interface AndroidMutationPlan {
  action: "clearData" | "uninstall";
  deviceId: string;
  deviceName: string;
  packageName: string;
  appName: string;
}

function riskTone(risk?: string) {
  if (risk === "dangerous") return "danger" as const;
  if (risk === "sensitive") return "warning" as const;
  return "neutral" as const;
}

function PackageIcon({ analysis }: { analysis: PackageAnalysis }) {
  return analysis.iconDataUrl
    ? <img className="package-icon-image" src={analysis.iconDataUrl} alt={`${analysis.appName} 图标`} />
    : <span className={`package-icon-fallback ${analysis.platform}`}><PackageOpen size={30} /></span>;
}

export default function AppsPage({ activeDevice, iosSshReady = false, iosSession, defaultExportDirectory, initialView = "package", notify, record }: AppsProps) {
  const explicitViewChanged = appsPageCache.lastIntent !== undefined && appsPageCache.lastIntent !== initialView;
  const [view, setViewState] = useState<AppsView>(() => explicitViewChanged ? initialView : appsPageCache.view ?? initialView);
  const [analysis, setAnalysisState] = useState<PackageAnalysis | undefined>(() => appsPageCache.analysis);
  const [analyzing, setAnalyzing] = useState(false);
  const [installPlan, setInstallPlan] = useState<InstallPlan>();
  const [installing, setInstalling] = useState(false);
  const [installed, setInstalled] = useState<InstalledApp[]>(() => activeDevice ? appsPageCache.installedByDevice.get(activeDevice.id) ?? [] : []);
  const [installedLoading, setInstalledLoading] = useState(false);
  const [installedDeviceId, setInstalledDeviceId] = useState(activeDevice?.id ?? "");
  const [appSearch, setAppSearchState] = useState(appsPageCache.appSearch);
  const [exporting, setExporting] = useState<string>();
  const [iosCapabilities, setIosCapabilities] = useState<IosAppCapabilities>();
  const [iosCapabilitiesLoading, setIosCapabilitiesLoading] = useState(false);
  const [iosCapabilitiesError, setIosCapabilitiesError] = useState<string>();
  const [iosInstallOverride, setIosInstallOverride] = useState<"usb" | "ssh">();
  const [androidActionBusy, setAndroidActionBusy] = useState<string>();
  const [androidMutationPlan, setAndroidMutationPlan] = useState<AndroidMutationPlan>();
  const [androidMutationBusy, setAndroidMutationBusy] = useState(false);

  const androidReady = activeDevice?.platform === "android" && activeDevice.state === "online";
  const iosSshVerified = !!activeDevice?.jailbroken || !!iosSession?.jailbreakConfirmed || iosSshReady;
  const iosRootReady = iosSshVerified && iosSession?.connected && iosSession.remoteUid === 0;
  const iosUsbInstallReady = activeDevice?.platform === "ios" && activeDevice.state === "online" && activeDevice.transport === "usbmux" && activeDevice.connectionSource !== "manual";
  const iosSshInstallReady = !!iosRootReady && !!iosCapabilities?.installers.length;
  const iosInstallMethod = iosInstallOverride === "usb" && iosUsbInstallReady ? "usb" : iosInstallOverride === "ssh" && iosSshInstallReady ? "ssh" : iosUsbInstallReady ? "usb" : "ssh";
  const activeDeviceIdRef = useRef(activeDevice?.id);
  const installedRequestRef = useRef(0);
  activeDeviceIdRef.current = activeDevice?.id;
  const setView = (next: AppsView) => { appsPageCache.view = next; setViewState(next); };
  const setAnalysis = (next: PackageAnalysis | undefined) => { appsPageCache.analysis = next; setAnalysisState(next); };
  const setAppSearch = (next: string) => { appsPageCache.appSearch = next; setAppSearchState(next); };

  useEffect(() => {
    appsPageCache.lastIntent = initialView;
    appsPageCache.view = view;
  }, [initialView, view]);

  useEffect(() => {
    setIosInstallOverride(undefined);
  }, [activeDevice?.id]);

  useEffect(() => {
    let cancelled = false;
    if (activeDevice?.platform !== "ios" || !iosRootReady || !iosSession) {
      setIosCapabilities(undefined);
      setIosCapabilitiesError(undefined);
      setIosCapabilitiesLoading(false);
      return;
    }
    setIosCapabilitiesLoading(true);
    setIosCapabilitiesError(undefined);
    void api.iosAppCapabilities(iosSession.sessionId)
      .then((result) => { if (!cancelled) setIosCapabilities(result); })
      .catch((error) => { if (!cancelled) setIosCapabilitiesError(error instanceof Error ? error.message : String(error)); })
      .finally(() => { if (!cancelled) setIosCapabilitiesLoading(false); });
    return () => { cancelled = true; };
  }, [activeDevice?.platform, iosRootReady, iosSession]);

  const loadInstalled = useCallback(async () => {
    const requestDevice = activeDevice;
    const requestNumber = ++installedRequestRef.current;
    const canReadIos = requestDevice?.platform === "ios" && !!iosRootReady && !!iosSession;
    if ((!androidReady && !canReadIos) || !requestDevice) {
      setInstalledDeviceId(requestDevice?.id ?? "");
      setInstalled([]);
      setInstalledLoading(false);
      return;
    }
    const requestDeviceId = requestDevice.id;
    const cached = appsPageCache.installedByDevice.get(requestDeviceId);
    setInstalledDeviceId(requestDeviceId);
    setInstalled(cached ?? []);
    setInstalledLoading(true);
    try {
      let result: InstalledApp[];
      if (requestDevice.platform === "ios" && iosSession) {
        const apps = await api.iosInstalledApps(iosSession.sessionId, "all", 300);
        result = apps.map((app) => ({
          packageName: app.bundleId,
          appName: app.displayName,
          versionName: app.versionName,
          versionCode: app.buildVersion,
          system: app.system,
          paths: [app.appPath],
        }));
      } else {
        result = await api.installedApps(requestDeviceId);
      }
      appsPageCache.installedByDevice.set(requestDeviceId, result);
      if (activeDeviceIdRef.current !== requestDeviceId || installedRequestRef.current !== requestNumber) return;
      setInstalledDeviceId(requestDeviceId);
      setInstalled(result);
    } catch (error) {
      if (activeDeviceIdRef.current !== requestDeviceId || installedRequestRef.current !== requestNumber) return;
      notify("error", "无法读取应用列表", error instanceof Error ? error.message : String(error));
    } finally {
      if (activeDeviceIdRef.current === requestDeviceId && installedRequestRef.current === requestNumber) setInstalledLoading(false);
    }
  }, [activeDevice, androidReady, iosRootReady, iosSession, notify]);

  useEffect(() => { void loadInstalled(); }, [loadInstalled]);

  const analyzeSelection = useCallback(async (selection: SelectedPackageFile | null) => {
    if (!selection) return;
    if (!/\.(apk|ipa)$/i.test(selection.name)) {
      notify("error", "不支持的文件", "请选择扩展名为 .apk 或 .ipa 的移动应用包。");
      return;
    }
    setAnalyzing(true);
    try {
      const result = await api.analyzePackage(selection);
      setAnalysis(result);
      setView("package");
      notify("success", "应用包解析完成", `${result.appName} · ${result.packageName}`);
      record("解析移动应用包", `${result.fileName} · MD5 ${result.md5}`);
    } catch (error) {
      notify("error", "无法解析应用包", error instanceof Error ? error.message : String(error));
    } finally { setAnalyzing(false); }
  }, [notify, record]);

  const chooseAndAnalyze = async () => analyzeSelection(await choosePackageFile());

  const dropPackage = (event: DragEvent<HTMLButtonElement>) => {
    event.preventDefault();
    const file = event.dataTransfer.files?.[0];
    if (file) void analyzeSelection(packageSelectionFromFile(file));
  };

  useEffect(() => {
    if (!runningInDesktop()) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void import("@tauri-apps/api/webview")
      .then(({ getCurrentWebview }) => getCurrentWebview().onDragDropEvent((event) => {
        if (event.payload.type !== "drop") return;
        const path = event.payload.paths.find((candidate) => /\.(apk|ipa)$/i.test(candidate));
        if (path) void analyzeSelection({ path, name: path.split(/[\\/]/).pop() ?? path });
      }))
      .then((release) => { if (disposed) release(); else unlisten = release; })
      .catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, [analyzeSelection]);

  const canInstall = !!analysis && !analysis.previewOnly && !!activeDevice && activeDevice.state === "online" && analysis.platform === activeDevice.platform && (analysis.platform === "android" || iosUsbInstallReady || iosSshInstallReady);

  const prepareInstall = () => {
    if (!analysis || !activeDevice || !canInstall) return;
    const method = analysis.platform === "android" ? "android" : iosInstallMethod;
    setInstallPlan({
      deviceId: activeDevice.id,
      deviceName: activeDevice.name,
      platform: analysis.platform,
      packagePath: analysis.path,
      fileName: analysis.fileName,
      appName: analysis.appName,
      packageName: analysis.packageName,
      method,
      iosSessionId: method === "ssh" ? iosSession?.sessionId : undefined,
      iosInstallerId: method === "ssh" ? iosCapabilities?.preferredInstaller?.id : undefined,
    });
  };

  const install = async () => {
    const plan = installPlan;
    if (!plan) return;
    const targetChanged = !activeDevice
      || activeDevice.id !== plan.deviceId
      || activeDevice.state !== "online"
      || analysis?.path !== plan.packagePath
      || analysis.platform !== plan.platform
      || (plan.method === "ssh" && iosSession?.sessionId !== plan.iosSessionId);
    if (targetChanged) {
      setInstallPlan(undefined);
      notify("warning", "安装已取消", "确认期间当前设备、SSH 会话或应用包已变化；请在新目标上重新确认。");
      return;
    }
    if (plan.platform === "ios" && plan.method === "ssh" && !plan.iosSessionId) {
      notify("error", "设备端 IPA 安装器不可用", "请先建立 Root SSH 会话，并确认测试机已自行安装 appinst 或 IPA Installer Console。");
      return;
    }
    setInstalling(true);
    try {
      const result = plan.platform === "ios" && plan.method === "ssh" && plan.iosSessionId
        ? await api.installIosPackageSsh(plan.iosSessionId, plan.packagePath, plan.iosInstallerId)
        : await api.installPackage(plan.deviceId, plan.platform, plan.packagePath);
      if (!result.success) throw new Error(result.message);
      notify("success", "应用安装完成", `${plan.appName} → ${plan.deviceName}`);
      record("安装移动应用", `${plan.fileName} → ${plan.deviceName}`);
      setInstallPlan(undefined);
      if (plan.platform === "android" || iosRootReady) await loadInstalled();
    } catch (error) { notify("error", "应用安装失败", error instanceof Error ? error.message : String(error)); }
    finally { setInstalling(false); }
  };

  const exportApp = async (app: InstalledApp) => {
    if (!activeDevice) return;
    const destination = defaultExportDirectory?.trim() || await chooseDirectory(`导出 ${app.packageName}`);
    if (!destination) return;
    setExporting(app.packageName);
    try {
      const result = activeDevice.platform === "ios"
        ? iosSession && app.paths?.[0]
          ? await api.exportIosAppBundle(iosSession.sessionId, app.packageName, app.paths[0], destination)
          : undefined
        : await api.exportAndroidPackage(activeDevice.id, app.packageName, destination);
      if (!result) throw new Error("iOS .app 导出需要已验证的 Root SSH 会话和有效应用路径。");
      if (!result.success) throw new Error(result.message);
      const files = "localPath" in result ? result.localPath : result.files?.join("、") || destination;
      notify("success", activeDevice.platform === "ios" ? ".app 分析归档已导出" : "安装包已导出", files);
      record(activeDevice.platform === "ios" ? "导出 iOS .app 分析归档" : "导出 Android 安装包", `${app.packageName} → ${destination}`);
    } catch (error) { notify("error", "导出失败", error instanceof Error ? error.message : String(error)); }
    finally { setExporting(undefined); }
  };

  const copyPackageName = async (packageName: string) => {
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(packageName);
      } else {
        const input = document.createElement("textarea");
        input.value = packageName;
        input.style.position = "fixed";
        input.style.opacity = "0";
        document.body.appendChild(input);
        input.select();
        const copied = document.execCommand("copy");
        input.remove();
        if (!copied) throw new Error("当前系统未允许写入剪贴板");
      }
      notify("success", "包名已复制", packageName);
    } catch (error) {
      notify("error", "复制失败", error instanceof Error ? error.message : String(error));
    }
  };

  const runAndroidQuickAction = async (app: InstalledApp, action: "launch" | "forceStop") => {
    const target = activeDevice;
    if (!target || target.platform !== "android" || target.state !== "online" || androidActionBusy) return;
    const busyKey = `${target.id}:${app.packageName}:${action}`;
    setAndroidActionBusy(busyKey);
    try {
      const result = action === "launch"
        ? await api.launchAndroidApp(target.id, app.packageName)
        : await api.forceStopAndroidApp(target.id, app.packageName);
      if (!result.success || result.serial !== target.id || result.packageName !== app.packageName || result.action !== action) {
        throw new Error("设备返回的应用操作结果与已锁定目标不一致。");
      }
      const title = action === "launch" ? "应用已启动" : "应用已停止";
      notify("success", title, `${app.appName ?? app.packageName} · ${target.name}`);
      record(title, `${app.packageName} → ${target.name}`);
    } catch (error) {
      notify("error", action === "launch" ? "启动失败" : "停止失败", error instanceof Error ? error.message : String(error));
    } finally {
      setAndroidActionBusy((current) => current === busyKey ? undefined : current);
    }
  };

  const prepareAndroidMutation = (app: InstalledApp, action: AndroidMutationPlan["action"]) => {
    const target = activeDevice;
    if (!target || target.platform !== "android" || target.state !== "online" || app.system) return;
    setAndroidMutationPlan({
      action,
      deviceId: target.id,
      deviceName: target.name,
      packageName: app.packageName,
      appName: app.appName ?? app.packageName,
    });
  };

  const handleAndroidMoreAction = (app: InstalledApp, action: string) => {
    if (action === "export") void exportApp(app);
    else if (action === "clearData") prepareAndroidMutation(app, "clearData");
    else if (action === "uninstall") prepareAndroidMutation(app, "uninstall");
  };

  const confirmAndroidMutation = async () => {
    const plan = androidMutationPlan;
    if (!plan || androidMutationBusy) return;
    const cachedTarget = appsPageCache.installedByDevice.get(plan.deviceId)?.find((app) => app.packageName === plan.packageName);
    if (!activeDevice || activeDevice.id !== plan.deviceId || activeDevice.platform !== "android" || activeDevice.state !== "online" || !cachedTarget || cachedTarget.system) {
      setAndroidMutationPlan(undefined);
      notify("warning", "操作已取消", "确认期间设备、应用清单或系统应用状态已变化，请在当前目标上重新确认。");
      return;
    }
    setAndroidMutationBusy(true);
    const expectedAction = plan.action;
    try {
      const result = plan.action === "clearData"
        ? await api.clearAndroidAppData(plan.deviceId, plan.packageName)
        : await api.uninstallAndroidApp(plan.deviceId, plan.packageName);
      if (!result.success || result.serial !== plan.deviceId || result.packageName !== plan.packageName || result.action !== expectedAction) {
        throw new Error("设备返回的应用操作结果与已锁定目标不一致。");
      }
      const title = plan.action === "clearData" ? "应用数据已清除" : "应用已卸载";
      notify("success", title, `${plan.appName} · ${plan.deviceName}`);
      record(title, `${plan.packageName} → ${plan.deviceName}`);
      setAndroidMutationPlan(undefined);
      await loadInstalled();
    } catch (error) {
      notify("error", plan.action === "clearData" ? "清除数据失败" : "卸载失败", error instanceof Error ? error.message : String(error));
    } finally {
      setAndroidMutationBusy(false);
    }
  };

  const visibleInstalled = installedDeviceId === activeDevice?.id ? installed : [];
  const visibleInstalledLoading = installedDeviceId === activeDevice?.id && installedLoading;
  const filteredApps = useMemo(() => visibleInstalled.filter((app) => `${app.appName ?? ""} ${app.packageName}`.toLowerCase().includes(appSearch.toLowerCase())), [visibleInstalled, appSearch]);
  const installedReady = androidReady || !!iosRootReady;
  const installBlockedReason = analysis?.previewOnly
    ? "浏览器预览只验证选择和摘要；请打开桌面应用完成真实解析与安装。"
    : !activeDevice
      ? "选择在线设备后可安装。"
      : analysis?.platform !== activeDevice.platform
        ? "包格式与当前设备平台不匹配。"
        : analysis?.platform === "ios" && iosCapabilitiesLoading && !iosUsbInstallReady
          ? "正在自动探测设备端 IPA 安装能力…"
          : analysis?.platform === "ios" && !iosUsbInstallReady && !iosRootReady
            ? "局域网 iOS 请先在“文件”页建立 Root SSH 会话；USB iOS 也可直接使用 ideviceinstaller。"
            : analysis?.platform === "ios" && !iosUsbInstallReady && !iosSshInstallReady
              ? "测试机未检测到受支持的 appinst / IPA Installer Console；Mobius 不会自动安装设备工具。"
              : "设备当前不在线。";

  return (
    <div className="page apps-page">
      <div className="page-heading">
        <div><span className="eyebrow">APPLICATION WORKBENCH</span><h1>应用</h1><p>APK / IPA 静态信息、安装、设备应用清单与安装包导出。</p></div>
        <Button variant="primary" icon={analyzing ? <LoaderCircle className="spin" size={15} /> : <PackageOpen size={15} />} disabled={analyzing} onClick={chooseAndAnalyze}>{analyzing ? "正在解析…" : "选择 APK / IPA"}</Button>
      </div>
      <Tabs value={view} onChange={setView} options={[{ id: "package", label: "本地包解析与安装" }, { id: "installed", label: "设备应用与导出" }]} />
      {!runningInDesktop() && <InlineNotice tone="info" title="当前是浏览器界面预览">可以真实选择本机 APK/IPA，并计算文件摘要；完整清单解析、安装和导出只会在 Mobius 桌面应用中调用本机工具。</InlineNotice>}
      {activeDevice?.platform === "ios" && iosRootReady && <div className="ios-app-capability-strip">
        <span><StatusBadge tone="success">ROOT SSH</StatusBadge> {iosSession?.mode === "usb" ? "USB 直连" : "LAN SSH"}</span>
        <span><StatusBadge tone={iosCapabilities?.installers.length ? "success" : iosCapabilitiesLoading ? "neutral" : "warning"}>{iosCapabilitiesLoading ? "探测中" : iosCapabilities?.preferredInstaller?.name ?? "无设备安装器"}</StatusBadge> IPA</span>
        <span><StatusBadge tone={iosCapabilities?.listingAvailable ? "success" : "warning"}>{iosCapabilities?.listingAvailable ? "可用" : "待检测"}</StatusBadge> 应用清单</span>
        <span><StatusBadge tone={iosCapabilities?.exportAvailable ? "success" : "warning"}>{iosCapabilities?.exportAvailable ? "可用" : "待检测"}</StatusBadge> .app 导出</span>
      </div>}
      {activeDevice?.platform === "ios" && iosCapabilitiesError && <InlineNotice tone="warning" title="iOS 应用能力探测失败">{iosCapabilitiesError}</InlineNotice>}

      {view === "package" && (analysis ? <div className="package-layout">
        <Panel className="package-summary span-5">
          <div className="package-identity"><PackageIcon analysis={analysis} /><div><span className="eyebrow">{analysis.platform === "android" ? "ANDROID APK" : "IOS IPA"}</span><h2>{analysis.appName}</h2><code>{analysis.packageName}</code><div className="badge-row"><StatusBadge tone={analysis.platform === "android" ? "success" : "info"}>{analysis.platform.toUpperCase()}</StatusBadge>{analysis.debuggable && <StatusBadge tone="danger">DEBUGGABLE</StatusBadge>}</div></div></div>
          <div className="package-facts">
            <div><span>版本</span><strong>{analysis.versionName ?? "—"}</strong><small>Build {analysis.versionCode ?? "—"}</small></div>
            <div><span>系统范围</span><strong>{analysis.minOsVersion ?? "—"} → {analysis.targetOsVersion ?? "—"}</strong><small>{analysis.platform === "android" ? "minSdk → targetSdk" : "MinimumOSVersion"}</small></div>
            <div><span>文件大小</span><strong>{formatBytes(analysis.fileSize)}</strong><small>{analysis.fileName}</small></div>
            <div><span>架构</span><strong>{analysis.architectures.join(" · ") || "未识别"}</strong><small>{analysis.architectures.length} 种 ABI</small></div>
          </div>
          <div className="hash-block"><Fingerprint size={16} /><span><small>MD5</small><code>{analysis.md5}</code></span></div>
          {analysis.platform === "ios" && iosUsbInstallReady && iosSshInstallReady && <label className="ios-install-method"><span>安装通道</span><select value={iosInstallMethod} onChange={(event) => setIosInstallOverride(event.target.value as "usb" | "ssh")}><option value="usb">USB · ideviceinstaller（默认）</option><option value="ssh">SSH · {iosCapabilities?.preferredInstaller?.name}</option></select></label>}
          {analysis.platform === "ios" && canInstall && !(iosUsbInstallReady && iosSshInstallReady) && <div className="ios-install-auto"><StatusBadge tone="info">AUTO</StatusBadge><span>{iosInstallMethod === "usb" ? "USB · ideviceinstaller" : `SSH · ${iosCapabilities?.preferredInstaller?.name ?? "设备安装器"}`}</span></div>}
          <div className="package-actions"><Button variant="primary" icon={<Upload size={15} />} disabled={!canInstall} onClick={prepareInstall}>安装到当前设备</Button><Button icon={<PackageOpen size={15} />} onClick={chooseAndAnalyze}>换一个包</Button></div>
          {!canInstall && <p className="capability-note"><Info size={14} /> {installBlockedReason}</p>}
        </Panel>
        <div className="package-detail-stack span-7">
          <Panel title={<><ShieldCheck size={17} /> 权限与隐私声明</>} action={<span className="panel-summary">{analysis.permissions.length} 项</span>}>
            {analysis.permissions.length ? <div className="permission-list">{analysis.permissions.map((permission) => <div key={permission.name}><span className={`permission-risk risk-${permission.risk ?? "unknown"}`}><ShieldAlert size={15} /></span><div><strong>{permission.label ?? permission.name.split(".").pop()}</strong><code>{permission.name}</code>{permission.usageDescription && <p>{permission.usageDescription}</p>}</div><StatusBadge tone={riskTone(permission.risk)}>{permission.risk === "dangerous" ? "高关注" : permission.risk === "sensitive" ? "敏感" : "常规"}</StatusBadge></div>)}</div> : <EmptyState icon={<ShieldCheck size={25} />} title="未声明权限" detail="解析器没有从清单中发现权限或隐私用途说明。" />}
          </Panel>
          <Panel title={<><AppWindow size={17} /> 清单与解析状态</>}>
            <div className="component-summary"><div><strong>{analysis.platform === "android" ? "APK" : "IPA"}</strong><span>包格式</span></div><div><strong>{analysis.source ?? "内置解析"}</strong><span>元数据来源</span></div><div><strong>{analysis.fallbackUsed ? "降级结果" : "完整结果"}</strong><span>解析状态</span></div></div>
            {analysis.signature && <div className="signature-block"><FileKey2 size={17} /><div><strong>{analysis.signature.subject ?? "签名主体未提供"}</strong><span>{analysis.signature.issuer && `签发者：${analysis.signature.issuer}`}</span><code>{analysis.signature.sha256 ?? "SHA-256 未提供"}</code></div></div>}
            {!!analysis.components?.length && <details className="component-details"><summary>查看组件明细 <ChevronDown size={14} /></summary>{analysis.components.map((component) => <div key={`${component.kind}-${component.name}`}><StatusBadge tone={component.exported ? "warning" : "neutral"}>{component.kind}</StatusBadge><code>{component.name}</code><span>{component.exported === undefined ? "—" : component.exported ? "exported" : "internal"}</span></div>)}</details>}
          </Panel>
          {!!analysis.warnings.length && <InlineNotice tone="warning" title="解析不完整">{analysis.warnings.join("；")}</InlineNotice>}
        </div>
      </div> : <button className="package-dropzone" onClick={chooseAndAnalyze} onDragOver={(event) => event.preventDefault()} onDrop={dropPackage} disabled={analyzing}>
        <span><FileArchive size={34} /></span><h2>{analyzing ? "正在解析包结构与清单…" : "点击选择，或拖入 APK / IPA"}</h2><p>整个区域均可点击。本地离线解析应用名称、包标识、版本、图标、MD5、权限、架构和可读取的清单信息。</p><small>文件不会上传；桌面端只读取你明确选择的文件。</small>
      </button>)}

      {view === "installed" && <Panel className="installed-apps-panel" title={<><Smartphone size={17} /> {activeDevice ? `${activeDevice.name} 的应用` : "设备应用"}</>} action={<Button variant="ghost" icon={<RefreshCw className={visibleInstalledLoading ? "spin" : ""} size={14} />} disabled={!installedReady || visibleInstalledLoading} onClick={loadInstalled}>刷新</Button>}>
        {!installedReady ? <EmptyState icon={<ArchiveRestore size={28} />} title={activeDevice?.platform === "ios" ? "请先连接 Root SSH" : "请选择在线设备"} detail={activeDevice?.platform === "ios" ? "验证 SSH 后会自动读取用户与系统 .app 清单，无需再输入命令。" : "读取和导出安装包需要明确的在线目标。"} /> : <>
          {activeDevice?.platform === "ios" && <InlineNotice tone="info" title="iOS 导出边界">导出为 `.app` 开发分析 `tar.gz`，不是可安装 IPA；不处理可执行文件保护状态，不重建签名或描述文件。</InlineNotice>}
          <div className="installed-toolbar"><div className="search-input"><Search size={15} /><input value={appSearch} onChange={(event) => setAppSearch(event.target.value)} placeholder="按应用名或包名筛选" /></div><span>{filteredApps.length} 个应用</span></div>
          {filteredApps.length ? <div className="installed-list"><div className="installed-row installed-head"><span>应用</span><span>版本</span><span>类型</span><span>快捷操作</span></div>{filteredApps.map((app) => {
            const targetKey = `${activeDevice?.id ?? ""}:${app.packageName}`;
            const rowBusy = androidActionBusy?.startsWith(`${targetKey}:`) ?? false;
            return <div className="installed-row" key={app.packageName}><span><span className="mini-app-icon"><Box size={16} /></span><span><strong>{app.appName ?? app.packageName}</strong><code title={app.paths?.[0]}>{app.packageName}</code></span></span><span><strong>{app.versionName ?? "—"}</strong><small>Build {app.versionCode ?? "—"}</small></span><span><StatusBadge tone={app.debuggable ? "warning" : "neutral"}>{app.debuggable ? "DEBUGGABLE" : app.system ? "SYSTEM" : "USER"}</StatusBadge></span>{activeDevice?.platform === "android" ? <div className="installed-actions">
              <Button className="app-row-icon" variant="ghost" icon={<Copy size={14} />} disabled={rowBusy || androidMutationBusy} title={`复制 ${app.packageName}`} aria-label={`复制 ${app.packageName}`} onClick={() => void copyPackageName(app.packageName)} />
              <Button className="app-row-action" icon={androidActionBusy === `${targetKey}:launch` ? <LoaderCircle className="spin" size={14} /> : <Play size={14} />} disabled={!!androidActionBusy || androidMutationBusy} onClick={() => void runAndroidQuickAction(app, "launch")}>启动</Button>
              <Button className="app-row-action" icon={androidActionBusy === `${targetKey}:forceStop` ? <LoaderCircle className="spin" size={14} /> : <Square size={12} />} disabled={!!androidActionBusy || androidMutationBusy} onClick={() => void runAndroidQuickAction(app, "forceStop")}>停止</Button>
              <select className="installed-action-select" defaultValue="" disabled={!!exporting || !!androidActionBusy || androidMutationBusy} title={app.system ? "更多操作；系统应用已保护" : "更多应用操作"} aria-label={`更多 ${app.packageName} 操作`} onChange={(event) => { const action = event.currentTarget.value; event.currentTarget.value = ""; handleAndroidMoreAction(app, action); }}>
                <option value="" disabled>更多…</option>
                <option value="export">导出 APK{(app.paths?.length ?? 0) > 1 ? " 组" : ""}</option>
                <option value="clearData" disabled={app.system}>清除应用数据{app.system ? "（系统应用已保护）" : ""}</option>
                <option value="uninstall" disabled={app.system}>卸载应用{app.system ? "（系统应用已保护）" : ""}</option>
              </select>
            </div> : <Button icon={exporting === app.packageName ? <LoaderCircle className="spin" size={14} /> : <FolderDown size={14} />} disabled={!!exporting || !iosCapabilities?.exportAvailable} onClick={() => exportApp(app)}>导出 .app</Button>}</div>;
          })}</div> : !visibleInstalledLoading && <EmptyState icon={<ArchiveRestore size={25} />} title="未读取到应用" detail={activeDevice?.platform === "ios" ? "确认设备上的 plutil/base64 可用，然后刷新。" : "当前筛选没有匹配项。"} />}
        </>}
      </Panel>}

      {installPlan && <Modal title="安装应用到已确认设备？" subtitle={`${installPlan.fileName} → ${installPlan.deviceName}`} onClose={() => setInstallPlan(undefined)} footer={<><Button onClick={() => setInstallPlan(undefined)}>取消</Button><Button variant="primary" icon={installing ? <LoaderCircle className="spin" size={14} /> : <PackageCheck size={14} />} disabled={installing} onClick={install}>{installing ? "正在安装…" : "确认安装 / 更新"}</Button></>}>
        <div className="install-confirm"><span className={`package-icon-fallback ${installPlan.platform}`}><PackageOpen size={30} /></span><div><strong>{installPlan.appName}</strong><code>{installPlan.packageName}</code><p>{installPlan.platform === "ios" ? `已锁定 ${installPlan.method === "usb" ? "USB ideviceinstaller" : `SSH ${installPlan.iosInstallerId ?? "设备安装器"}`} 通道；签名、信任与 AppSync 兼容性由设备环境决定。` : "将使用替换安装模式保留应用数据；设备端仍可能显示系统确认界面。"}</p></div></div><InlineNotice tone="warning" title="目标已锁定">本次只会发送到 {installPlan.deviceName}（{installPlan.deviceId}）。若确认期间设备、应用包或 SSH 会话改变，安装会直接取消。</InlineNotice>
      </Modal>}
      {androidMutationPlan && <Modal title={androidMutationPlan.action === "clearData" ? "清除这个应用的数据？" : "从设备卸载这个应用？"} subtitle={`${androidMutationPlan.appName} → ${androidMutationPlan.deviceName}`} onClose={() => { if (!androidMutationBusy) setAndroidMutationPlan(undefined); }} footer={<><Button disabled={androidMutationBusy} onClick={() => setAndroidMutationPlan(undefined)}>取消</Button><Button variant="danger" icon={androidMutationBusy ? <LoaderCircle className="spin" size={14} /> : androidMutationPlan.action === "clearData" ? <Eraser size={14} /> : <Trash2 size={14} />} disabled={androidMutationBusy} onClick={() => void confirmAndroidMutation()}>{androidMutationBusy ? "正在处理…" : androidMutationPlan.action === "clearData" ? "确认清除数据" : "确认卸载"}</Button></>}>
        <div className="android-mutation-summary"><span className="mini-app-icon"><Box size={18} /></span><div><strong>{androidMutationPlan.appName}</strong><code>{androidMutationPlan.packageName}</code></div></div>
        <InlineNotice tone="warning" title="目标已锁定">{androidMutationPlan.action === "clearData" ? "将删除该应用在设备上的账号、设置、缓存和其他本地数据，但保留应用本身。" : "将从已锁定设备上移除应用及其数据。"}确认期间如果切换设备或应用状态变化，本次操作会自动取消。系统应用无法使用此操作。</InlineNotice>
      </Modal>}
    </div>
  );
}
