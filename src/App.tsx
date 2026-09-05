import { Activity, AppWindow, Camera, Cable, ChevronDown, CircleStop, Command, FolderSync, HelpCircle, KeyRound, LoaderCircle, Menu, Minus, MonitorSmartphone, Network, PanelLeftClose, Play, RefreshCw, Search, Settings as SettingsIcon, Smartphone, Sparkles, Square, TerminalSquare, Trash2, Wifi, Wrench, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { api } from "./lib/api";
import { chooseBinaryFile } from "./lib/dialog";
import { commandKey, shortTime } from "./lib/format";
import type { ActivityItem, AppSettings, Device, FridaServerResult, IosFridaServerResult, IosSshSession, PageKey, ToastMessage, ToolchainConfiguration, ToolHealth } from "./types";
import { Button, DeviceIdentity, Field, InlineNotice, Modal, StatusBadge, StatusDot, ToastStack } from "./components/Ui";
import Devices from "./pages/Devices";
import NetworkPage from "./pages/Network";
import FilesPage from "./pages/Files";
import AppsPage from "./pages/Apps";
import DebugPage, { type DebugView } from "./pages/Debug";
import DeviceConnect from "./pages/DeviceConnect";
import SettingsPage, { type SettingsGroup } from "./pages/Settings";

const navigation: Array<{ id: PageKey; label: string; icon: typeof MonitorSmartphone; shortcut: string }> = [
  { id: "devices", label: "工作台", icon: MonitorSmartphone, shortcut: "1" },
  { id: "apps", label: "应用", icon: AppWindow, shortcut: "2" },
  { id: "files", label: "文件", icon: FolderSync, shortcut: "3" },
  { id: "network", label: "网络", icon: Network, shortcut: "4" },
  { id: "debug", label: "调试", icon: TerminalSquare, shortcut: "5" },
  { id: "settings", label: "设置", icon: SettingsIcon, shortcut: "6" },
];

const defaultSettings: AppSettings = {
  adbPath: "",
  scrcpyPath: "",
  fridaPath: "",
  iosToolsPath: "",
  managedToolsPath: "",
  mediaDirectory: "",
  appExportDirectory: "",
  scanCidr: "",
  scanPort: "5555",
  proxyHost: "127.0.0.1",
  proxyPort: "8080",
  operationConfirmations: true,
  redactLogs: true,
  compactMode: false,
};

const manualIosStorageKey = "mobius.devices.ios-ssh";
const workspaceStorageKey = "mobius.workspace.v1";

function readWorkspaceState(): { activeId?: string; collapsed: boolean } {
  try {
    const stored = JSON.parse(localStorage.getItem(workspaceStorageKey) ?? "{}");
    return {
      activeId: typeof stored.activeId === "string" ? stored.activeId : undefined,
      collapsed: stored.collapsed === true,
    };
  } catch {
    return { collapsed: false };
  }
}

function readManualIosEndpoints(): Device[] {
  try {
    const stored = JSON.parse(localStorage.getItem(manualIosStorageKey) ?? "[]");
    if (!Array.isArray(stored)) return [];
    return stored.filter((item): item is Device => item
      && typeof item.id === "string"
      && item.id.startsWith("ios-ssh:")
      && typeof item.name === "string"
      && item.platform === "ios"
      && item.transport === "wifi"
      && typeof item.address === "string")
      .map((item) => ({ ...item, state: "registered", connectionSource: "manual" }));
  } catch {
    return [];
  }
}

function mergeDevices(discovered: Device[], registered: Device[]) {
  const discoveredIds = new Set(discovered.map((device) => device.id));
  const discoveredAddresses = new Set(discovered.flatMap((device) => device.address ? [`${device.platform}:${device.address.toLowerCase()}`] : []));
  const uniqueRegistered = registered.filter((device, index) => {
    const addressKey = device.address ? `${device.platform}:${device.address.toLowerCase()}` : "";
    return !discoveredIds.has(device.id)
      && (!addressKey || !discoveredAddresses.has(addressKey))
      && registered.findIndex((candidate) => candidate.id === device.id || (!!addressKey && `${candidate.platform}:${candidate.address?.toLowerCase()}` === addressKey)) === index;
  });
  return [...discovered, ...uniqueRegistered];
}

function configuredToolchain(settings: AppSettings): ToolchainConfiguration {
  const optional = (value: string) => value.trim() || undefined;
  return {
    adbPath: optional(settings.adbPath),
    scrcpyPath: optional(settings.scrcpyPath),
    fridaPath: optional(settings.fridaPath),
    iosToolsPath: optional(settings.iosToolsPath),
    managedToolsPath: optional(settings.managedToolsPath),
  };
}

type ActionModal = "scrcpy" | "frida" | "proxy" | null;
type NetworkIntent = "mapping" | "proxy";
type AppsIntent = "package" | "installed";
type DeviceIntent = "list" | "pair" | "manual" | "legacy" | "ios";
type UtilityModal = "activity" | "help" | null;

async function windowAction(action: "minimize" | "maximize" | "close") {
  if (!("__TAURI_INTERNALS__" in window)) return;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  const appWindow = getCurrentWindow();
  if (action === "minimize") await appWindow.minimize();
  else if (action === "maximize") await appWindow.toggleMaximize();
  else await appWindow.close();
}

export default function App() {
  const initialWorkspace = useMemo(readWorkspaceState, []);
  const [page, setPage] = useState<PageKey>("devices");
  const [collapsed, setCollapsed] = useState(initialWorkspace.collapsed);
  const [devices, setDevices] = useState<Device[]>([]);
  const [manualIosEndpoints, setManualIosEndpoints] = useState<Device[]>(readManualIosEndpoints);
  const [iosSshSessions, setIosSshSessions] = useState<Record<string, IosSshSession>>({});
  const [androidFridaResults, setAndroidFridaResults] = useState<Record<string, FridaServerResult>>({});
  const [iosFridaResults, setIosFridaResults] = useState<Record<string, IosFridaServerResult>>({});
  const [tools, setTools] = useState<ToolHealth[]>([]);
  const [toolHealthFailed, setToolHealthFailed] = useState(false);
  const [activeId, setActiveId] = useState<string | undefined>(initialWorkspace.activeId);
  const [loading, setLoading] = useState(true);
  const [toasts, setToasts] = useState<ToastMessage[]>([]);
  const [activities, setActivities] = useState<ActivityItem[]>([]);
  const [modal, setModal] = useState<ActionModal>(null);
  const [utilityModal, setUtilityModal] = useState<UtilityModal>(null);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [paletteIndex, setPaletteIndex] = useState(0);
  const [devicePickerOpen, setDevicePickerOpen] = useState(false);
  const [devicePickerIndex, setDevicePickerIndex] = useState(0);
  const devicePickerRef = useRef<HTMLDivElement>(null);
  const devicePickerTriggerRef = useRef<HTMLButtonElement>(null);
  const mainContentRef = useRef<HTMLElement>(null);
  const [networkIntent, setNetworkIntent] = useState<NetworkIntent>("proxy");
  const [appsIntent, setAppsIntent] = useState<AppsIntent>("package");
  const [deviceIntent, setDeviceIntent] = useState<DeviceIntent>("list");
  const [debugIntent, setDebugIntent] = useState<DebugView>("instrumentation");
  const [settingsGroup, setSettingsGroup] = useState<SettingsGroup>("toolchain");
  const [consoleSeed, setConsoleSeed] = useState("");
  const iosSessionHealthInFlight = useRef(new Set<string>());
  const [settings, setSettings] = useState<AppSettings>(() => {
    try { return { ...defaultSettings, ...JSON.parse(localStorage.getItem("mobius.settings") ?? "{}") }; }
    catch { return defaultSettings; }
  });
  const presentedDevices = useMemo(() => mergeDevices(devices, manualIosEndpoints).map((device) => iosSshSessions[device.id]?.jailbreakConfirmed ? { ...device, state: "online" as const, jailbroken: true } : device), [devices, manualIosEndpoints, iosSshSessions]);
  const pickerDevices = useMemo(() => [...presentedDevices].sort((left, right) => {
    const priority: Record<Device["state"], number> = { online: 0, connecting: 1, registered: 2, unauthorized: 3, offline: 4 };
    return priority[left.state] - priority[right.state];
  }), [presentedDevices]);
  const activeDevice = presentedDevices.find((device) => device.id === activeId) ?? presentedDevices.find((device) => device.state === "online") ?? presentedDevices.find((device) => device.state === "registered");
  const activeIosSshSession = activeDevice ? iosSshSessions[activeDevice.id] : undefined;
  const activeAndroidFridaResult = activeDevice ? androidFridaResults[activeDevice.id] : undefined;
  const activeIosFridaResult = activeDevice ? iosFridaResults[activeDevice.id] : undefined;
  const macPlatform = navigator.platform.toLowerCase().includes("mac");

  const notify = useCallback((type: ToastMessage["type"], title: string, detail?: string) => {
    const id = Date.now() + Math.random();
    setToasts((current) => [...current, { id, type, title, detail }]);
    window.setTimeout(() => setToasts((current) => current.filter((item) => item.id !== id)), type === "error" ? 9000 : type === "warning" ? 6500 : 4500);
  }, []);

  const record = useCallback((title: string, detail: string, status: ActivityItem["status"] = "success") => {
    setActivities((current) => [{ id: crypto.randomUUID(), title, detail, status, at: shortTime() }, ...current].slice(0, 30));
  }, []);

  const updateIosSshSession = useCallback((deviceId: string, session?: IosSshSession) => {
    setIosSshSessions((current) => {
      if (session) return { ...current, [deviceId]: session };
      const next = { ...current };
      delete next[deviceId];
      return next;
    });
    setIosFridaResults((current) => {
      const existing = current[deviceId];
      if (session && (!existing || existing.sessionId === session.sessionId)) return current;
      const next = { ...current };
      delete next[deviceId];
      return next;
    });
  }, []);

  const updateMediaDirectory = useCallback((directory: string) => {
    setSettings((current) => {
      const next = { ...current, mediaDirectory: directory };
      localStorage.setItem("mobius.settings", JSON.stringify(next));
      return next;
    });
  }, []);

  const registerIosEndpoint = useCallback((device: Device) => {
    setManualIosEndpoints((current) => {
      const next = [device, ...current.filter((item) => item.id !== device.id && item.address?.toLowerCase() !== device.address?.toLowerCase())];
      localStorage.setItem(manualIosStorageKey, JSON.stringify(next));
      return next;
    });
    setActiveId(device.id);
    setPage("files");
    notify("success", "iOS SSH 端点已登记", `${device.name} · ${device.address} · 正在打开文件连接`);
  }, [notify]);

  const forgetIosEndpoint = useCallback((deviceId: string) => {
    setManualIosEndpoints((current) => {
      const next = current.filter((device) => device.id !== deviceId);
      localStorage.setItem(manualIosStorageKey, JSON.stringify(next));
      return next;
    });
    const session = iosSshSessions[deviceId];
    if (session) void api.stopIosSshSession(session.sessionId).catch(() => undefined);
    updateIosSshSession(deviceId, undefined);
    setActiveId((current) => current === deviceId ? devices.find((device) => device.state === "online")?.id : current);
    notify("success", "已忘记 iOS SSH 端点", "只移除了本机登记信息；USB 自动识别设备不受影响。");
  }, [devices, iosSshSessions, notify, updateIosSshSession]);

  const refresh = useCallback(async () => {
    setLoading(true);
    const [deviceResult, toolResult] = await Promise.allSettled([api.devices(), api.toolHealth()]);
    if (deviceResult.status === "fulfilled") {
      setDevices(deviceResult.value);
      const combined = mergeDevices(deviceResult.value, manualIosEndpoints);
      setActiveId((current) => current && combined.some((device) => device.id === current) ? current : combined.find((device) => device.state === "online")?.id ?? combined.find((device) => device.state === "registered")?.id);
    } else notify("error", "设备检测失败", String(deviceResult.reason));
    if (toolResult.status === "fulfilled") {
      setTools(toolResult.value);
      setToolHealthFailed(false);
    } else {
      setToolHealthFailed(true);
      notify("error", "工具链检测失败", String(toolResult.reason));
    }
    setLoading(false);
  }, [manualIosEndpoints, notify]);

  const refreshDevicesQuietly = useCallback(async () => {
    if (document.hidden) return;
    try {
      const discovered = await api.devices();
      setDevices(discovered);
      const combined = mergeDevices(discovered, manualIosEndpoints);
      setActiveId((current) => current && combined.some((device) => device.id === current) ? current : combined.find((device) => device.state === "online")?.id ?? combined[0]?.id);
    } catch {
      // Background hot-plug polling is intentionally quiet; an explicit refresh reports errors.
    }
  }, [manualIosEndpoints]);

  useEffect(() => {
    void (async () => {
      try { await api.configureToolchain(configuredToolchain(settings)); }
      catch (error) { notify("warning", "工具链配置未应用", error instanceof Error ? error.message : String(error)); }
      await refresh();
    })();
  }, [refresh]);

  useEffect(() => {
    localStorage.setItem(workspaceStorageKey, JSON.stringify({ activeId, collapsed }));
  }, [activeId, collapsed]);

  useEffect(() => {
    const timer = window.setInterval(() => void refreshDevicesQuietly(), 10_000);
    return () => window.clearInterval(timer);
  }, [refreshDevicesQuietly]);

  useEffect(() => {
    const checkSessions = async () => {
      if (document.hidden) return;
      for (const [deviceId, session] of Object.entries(iosSshSessions)) {
        if (iosSessionHealthInFlight.current.has(session.sessionId)) continue;
        iosSessionHealthInFlight.current.add(session.sessionId);
        try {
          const result = await api.testIosSshConnection(session.sessionId);
          if (!result.connected) throw new Error(result.message || "SSH 会话已断开");
        } catch {
          setIosSshSessions((current) => {
            if (current[deviceId]?.sessionId !== session.sessionId) return current;
            const next = { ...current };
            delete next[deviceId];
            return next;
          });
          setIosFridaResults((current) => {
            if (current[deviceId]?.sessionId !== session.sessionId) return current;
            const next = { ...current };
            delete next[deviceId];
            return next;
          });
          void api.stopIosSshSession(session.sessionId).catch(() => undefined);
          notify("warning", "iOS SSH 已断开", "已移除过期会话；进入文件页可一键重连。");
          record("iOS SSH 会话已断开", deviceId, "warning");
        } finally {
          iosSessionHealthInFlight.current.delete(session.sessionId);
        }
      }
    };
    const timer = window.setInterval(() => void checkSessions(), 30_000);
    return () => window.clearInterval(timer);
  }, [iosSshSessions, notify, record]);

  useEffect(() => {
    const keyboard = (event: KeyboardEvent) => {
      const modifier = event.metaKey || event.ctrlKey;
      const overlayOpen = !!modal || !!utilityModal || !!document.querySelector(".modal-backdrop");
      if (event.key === "Escape") {
        setPaletteOpen(false);
        setDevicePickerOpen(false);
        if (!overlayOpen && page === "devices" && deviceIntent !== "list") {
          setDeviceIntent("list");
          mainContentRef.current?.scrollTo({ top: 0, behavior: "auto" });
        }
        return;
      }
      if (modifier && event.key.toLowerCase() === "k") {
        if (overlayOpen) return;
        event.preventDefault();
        setDevicePickerOpen(false);
        setPaletteOpen((value) => !value);
        return;
      }
      if (overlayOpen || paletteOpen) return;
      if (modifier && /^[1-6]$/.test(event.key)) {
        event.preventDefault();
        const next = navigation[Number(event.key) - 1].id;
        if (next === "devices") setDeviceIntent("list");
        setPage(next);
        setPaletteOpen(false);
        setDevicePickerOpen(false);
        mainContentRef.current?.scrollTo({ top: 0, behavior: "auto" });
      }
    };
    window.addEventListener("keydown", keyboard);
    return () => window.removeEventListener("keydown", keyboard);
  }, [deviceIntent, modal, page, paletteOpen, utilityModal]);

  useEffect(() => {
    if (!devicePickerOpen) return;
    const closeOnOutsideClick = (event: PointerEvent) => {
      if (!devicePickerRef.current?.contains(event.target as Node)) setDevicePickerOpen(false);
    };
    document.addEventListener("pointerdown", closeOnOutsideClick);
    return () => document.removeEventListener("pointerdown", closeOnOutsideClick);
  }, [devicePickerOpen]);

  useEffect(() => {
    if (!devicePickerOpen) return;
    window.requestAnimationFrame(() => devicePickerRef.current?.querySelector<HTMLButtonElement>(`[data-device-index="${devicePickerIndex}"]`)?.focus());
  }, [devicePickerIndex, devicePickerOpen]);

  useEffect(() => {
    if (modal || utilityModal || paletteOpen) setDevicePickerOpen(false);
  }, [modal, paletteOpen, utilityModal]);

  useEffect(() => {
    mainContentRef.current?.scrollTo({ top: 0, behavior: "instant" });
  }, [appsIntent, debugIntent, deviceIntent, networkIntent, page, settingsGroup]);

  const openDevicePicker = (fromEnd = false) => {
    if (!pickerDevices.length || modal || utilityModal || paletteOpen) return;
    const selectedIndex = pickerDevices.findIndex((device) => device.id === activeDevice?.id);
    setDevicePickerIndex(selectedIndex >= 0 ? selectedIndex : fromEnd ? pickerDevices.length - 1 : 0);
    setDevicePickerOpen(true);
  };
  const closeDevicePicker = (restoreFocus = false) => {
    setDevicePickerOpen(false);
    if (restoreFocus) window.requestAnimationFrame(() => devicePickerTriggerRef.current?.focus());
  };
  const selectDeviceFromPicker = (device: Device) => {
    setActiveId(device.id);
    closeDevicePicker(true);
  };
  const devicePickerTriggerKey = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      openDevicePicker(event.key === "ArrowUp");
    } else if (event.key === "Escape" && devicePickerOpen) {
      event.preventDefault();
      closeDevicePicker();
    }
  };
  const devicePickerMenuKey = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!pickerDevices.length) return;
    let nextIndex: number | undefined;
    if (event.key === "ArrowDown") nextIndex = (devicePickerIndex + 1) % pickerDevices.length;
    else if (event.key === "ArrowUp") nextIndex = (devicePickerIndex - 1 + pickerDevices.length) % pickerDevices.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = pickerDevices.length - 1;
    else if (event.key === "Escape") {
      event.preventDefault();
      closeDevicePicker(true);
      return;
    } else if (event.key === "Tab") {
      setDevicePickerOpen(false);
      return;
    }
    if (nextIndex !== undefined) {
      event.preventDefault();
      setDevicePickerIndex(nextIndex);
    }
  };

  const scrollMainToTop = () => mainContentRef.current?.scrollTo({ top: 0, behavior: "auto" });
  const navigate = (next: PageKey) => { if (next === "devices") setDeviceIntent("list"); setPage(next); setPaletteOpen(false); setDevicePickerOpen(false); scrollMainToTop(); };
  const openDevices = (intent: DeviceIntent) => { setDeviceIntent(intent); setPage("devices"); setPaletteOpen(false); setDevicePickerOpen(false); scrollMainToTop(); };
  const openNetwork = (intent: NetworkIntent) => { setNetworkIntent(intent); setPage("network"); setPaletteOpen(false); };
  const openApps = (intent: AppsIntent) => { setAppsIntent(intent); setPage("apps"); setPaletteOpen(false); };
  const openDebug = (intent: DebugView, command = "") => { setDebugIntent(intent); setConsoleSeed(command); setPage("debug"); setPaletteOpen(false); };
  const quickAction = (action: "scan" | "connect" | "scrcpy" | "frida") => {
    if (action === "scan") openDevices("legacy");
    else if (action === "connect") openDevices("legacy");
    else if (!activeDevice) notify("warning", "请先选择设备");
    else if (activeDevice.state !== "online") notify("warning", "设备当前不可操作", "请先连接并完成调试授权。");
    else if (action === "scrcpy" && activeDevice.platform === "android") openDevices("list");
    else if (action === "frida" && activeDevice.platform === "ios") openDebug("instrumentation");
    else if (activeDevice.platform !== "android") notify("info", "此操作目前仅支持 Android 设备");
    else setModal(action);
  };
  const deviceAction = (action: "connect" | "pair" | "scan" | "ios" | "scrcpy" | "files" | "console" | "frida") => {
    if (action === "connect") openDevices("manual");
    else if (action === "pair") openDevices("pair");
    else if (action === "scan") openDevices("legacy");
    else if (action === "ios") openDevices("ios");
    else if (action === "files") navigate("files");
    else if (action === "console") openDebug("shell");
    else if (action === "scrcpy" && activeDevice?.platform === "android") setModal("scrcpy");
    else quickAction(action);
  };

  const captureToClipboard = useCallback(async () => {
    const target = activeDevice;
    if (!target || target.state !== "online") {
      notify("warning", "请选择在线设备");
      return;
    }
    try {
      const result = target.platform === "ios"
        ? await api.captureIosScreenshot(target.id, undefined, true)
        : await api.captureAndroidScreenshot(target.id, undefined, true);
      if (!result.success) throw new Error(result.message);
      notify("success", "截图已复制到剪贴板", `${target.name} · ${result.width ?? "?"}×${result.height ?? "?"}`);
      record("截图到剪贴板", `${target.name} · ${result.width ?? "?"}×${result.height ?? "?"}`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "截图失败", message);
      record("截图失败", `${target.name} · ${message}`, "error");
    }
  }, [activeDevice, notify, record]);

  const createDefaultReverse = useCallback(async () => {
    const target = activeDevice;
    const port = Number(settings.proxyPort);
    if (!target || target.platform !== "android" || target.state !== "online") {
      notify("warning", "请选择在线 Android 设备");
      return;
    }
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      notify("warning", "设置中的默认代理端口无效");
      return;
    }
    const endpoint = `tcp:${port}`;
    try {
      const existing = await api.mappings(target.id);
      if (existing.some((item) => item.direction === "reverse" && item.local === endpoint && item.remote === endpoint)) {
        notify("info", "USB Reverse 已存在", `设备 127.0.0.1:${port} → 本机 ${port}；系统代理未修改。`);
        return;
      }
      const result = await api.createMapping({ serial: target.id, direction: "reverse", local: endpoint, remote: endpoint });
      if (!result.success) throw new Error(result.message);
      notify("success", "USB Reverse 已创建", `设备 127.0.0.1:${port} → 本机 ${port}；系统代理未修改。`);
      record("创建代理 Reverse", `${target.name} · ${endpoint} · 未设置系统代理`);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "Reverse 创建失败", message);
      record("Reverse 创建失败", `${target.name} · ${message}`, "error");
    }
  }, [activeDevice, notify, record, settings.proxyPort]);

  const paletteItems = useMemo(() => {
    const deviceItems = activeDevice?.platform === "android" ? [
      { id: "files-action", label: "打开 Android 文件", detail: activeDevice.name, icon: FolderSync, run: () => navigate("files") },
      { id: "scrcpy-action", label: "打开 Android 内嵌投屏", detail: activeDevice.name, icon: MonitorSmartphone, run: () => quickAction("scrcpy") },
      { id: "screenshot-action", label: "截图到剪贴板", detail: activeDevice.name, icon: Camera, run: captureToClipboard },
      { id: "reverse-action", label: `仅创建 USB Reverse :${settings.proxyPort}`, detail: "不修改 Android 系统代理", icon: Cable, run: createDefaultReverse },
      { id: "frida-action", label: "打开 Android Frida 管理", detail: activeDevice.name, icon: KeyRound, run: () => quickAction("frida") },
    ] : activeDevice?.platform === "ios" ? [
      { id: "ios-files-action", label: activeIosSshSession ? "打开 iOS 文件" : "连接 iOS SSH", detail: activeDevice.name, icon: FolderSync, run: () => navigate("files") },
      { id: "ios-network-action", label: "打开 iOS 端口隧道", detail: "USB / SSH 转发", icon: Network, run: () => navigate("network") },
      { id: "ios-tools-action", label: "打开 iOS 系统工具", detail: "设备、进程、网络与日志", icon: Wrench, run: () => openDebug("system") },
      { id: "ios-screenshot-action", label: "截图到剪贴板", detail: activeDevice.name, icon: Camera, run: captureToClipboard },
      { id: "ios-frida-action", label: "打开 iOS Frida 管理", detail: activeDevice.name, icon: KeyRound, run: () => quickAction("frida") },
    ] : [];
    const items = [
      ...navigation.map((item) => ({ id: `page-${item.id}`, label: item.label, detail: `转到${item.label}`, icon: item.icon, run: () => navigate(item.id) })),
      ...pickerDevices.map((device) => ({ id: `device-${device.id}`, label: `切换设备：${device.name}`, detail: `${device.platform === "android" ? "Android" : "iOS"} · ${device.address ?? device.transport.toUpperCase()}`, icon: Smartphone, run: () => setActiveId(device.id) })),
      { id: "refresh", label: "刷新设备与工具链", detail: "重新检测本机环境", icon: RefreshCw, run: refresh },
      ...deviceItems,
    ];
    const query = paletteQuery.trim().toLowerCase();
    return items.filter((item) => !query || `${item.label} ${item.detail}`.toLowerCase().includes(query));
  }, [activeDevice, activeIosSshSession, captureToClipboard, createDefaultReverse, paletteQuery, pickerDevices, refresh, settings.proxyPort]);
  useEffect(() => { setPaletteIndex(0); }, [paletteQuery, paletteOpen]);
  const paletteKey = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") { event.preventDefault(); setPaletteIndex((current) => paletteItems.length ? (current + 1) % paletteItems.length : 0); }
    else if (event.key === "ArrowUp") { event.preventDefault(); setPaletteIndex((current) => paletteItems.length ? (current - 1 + paletteItems.length) % paletteItems.length : 0); }
    else if (event.key === "Enter" && paletteItems[paletteIndex]) { event.preventDefault(); void paletteItems[paletteIndex].run(); setPaletteOpen(false); }
  };

  const readyToolCount = tools.filter((tool) => tool.state === "ready").length;
  const toolIssues = tools.filter((tool) => tool.state !== "ready");
  const toolReady = (id: string) => tools.some((tool) => tool.id === id && tool.state === "ready");
  const activeCoreFailure = activeDevice?.platform === "ios"
    ? activeDevice.connectionSource === "manual"
      ? !toolReady("ssh") || !toolReady("scp")
      : !toolReady("ios")
    : !toolReady("adb");
  const toolHealthStatus = loading && !tools.length ? "running" : toolHealthFailed || !tools.length || activeCoreFailure ? "error" : toolIssues.length ? "warning" : "success";
  const toolHealthLabel = toolHealthFailed
    ? "工具链检测失败 · 显示上次结果"
    : loading && !tools.length
    ? "正在检测工具链"
    : !tools.length
      ? "工具链检测失败"
      : toolIssues.length
        ? `工具链 ${readyToolCount}/${tools.length} · ${toolIssues.length} 项待处理`
        : `工具链 ${readyToolCount}/${tools.length} · 全部就绪`;
  const toolHealthTitle = toolHealthFailed
    ? "本次工具链检测失败；点击查看配置并重新检测"
    : toolIssues.length
    ? `${toolIssues.map((tool) => tool.name).join("、")} 尚未就绪；点击前往设置`
    : "点击查看工具链设置";

  let content;
  if (page === "devices") content = <>
    <Devices devices={presentedDevices} activeDevice={activeDevice} loading={loading} onRefresh={refresh} onSelect={(id) => { setActiveId(id); scrollMainToTop(); }} onAction={deviceAction} onForgetRegisteredIos={forgetIosEndpoint} tools={tools} mediaDirectory={settings.mediaDirectory} onMediaDirectoryChange={updateMediaDirectory} notify={notify} record={record} />
    {deviceIntent !== "list" && <DeviceConnect key={deviceIntent} overlay initialMode={deviceIntent === "ios" ? "legacy" : deviceIntent} initialPlatform={deviceIntent === "ios" ? "ios" : "android"} defaultCidr={settings.scanCidr} defaultPorts={settings.scanPort} onBack={() => setDeviceIntent("list")} onRefreshDevices={refresh} onRegisterIosEndpoint={registerIosEndpoint} notify={notify} record={record} />}
  </>;
  else if (page === "network") content = <NetworkPage key={networkIntent} initialTab={networkIntent} activeDevice={activeDevice} defaultProxyHost={settings.proxyHost} defaultProxyPort={settings.proxyPort} iosSession={activeIosSshSession} tools={tools} onOpenIosFiles={() => navigate("files")} onOpenToolSettings={() => { setSettingsGroup("toolchain"); navigate("settings"); }} notify={notify} record={record} />;
  else if (page === "apps") content = <AppsPage key={appsIntent} initialView={appsIntent} activeDevice={activeDevice} iosSshReady={!!activeIosSshSession?.jailbreakConfirmed} iosSession={activeIosSshSession} defaultExportDirectory={settings.appExportDirectory} notify={notify} record={record} />;
  else if (page === "files") content = <FilesPage activeDevice={activeDevice} iosSession={activeIosSshSession} onIosSessionChange={(session) => activeDevice && updateIosSshSession(activeDevice.id, session)} notify={notify} record={record} />;
  else if (page === "debug") content = <DebugPage key={`${debugIntent}-${consoleSeed}`} initialView={debugIntent} initialCommand={consoleSeed} activeDevice={activeDevice} androidFridaResult={activeAndroidFridaResult} onAndroidFridaResultChange={(result) => { if (!activeDevice) return; setAndroidFridaResults((current) => { const next = { ...current }; if (result) next[activeDevice.id] = result; else delete next[activeDevice.id]; return next; }); }} iosSession={activeIosSshSession} iosFridaResult={activeIosFridaResult} onIosFridaResultChange={(result) => { if (!activeDevice) return; setIosFridaResults((current) => { const next = { ...current }; if (result) next[activeDevice.id] = result; else delete next[activeDevice.id]; return next; }); }} onOpenIosFiles={() => navigate("files")} onRefresh={refresh} onAction={quickAction} notify={notify} record={record} />;
  else content = <SettingsPage settings={settings} tools={tools} group={settingsGroup} onGroupChange={setSettingsGroup} onRefreshTools={refresh} onSave={(next) => { void (async () => { try { await api.configureToolchain(configuredToolchain(next)); setSettings(next); localStorage.setItem("mobius.settings", JSON.stringify(next)); document.body.classList.toggle("compact", next.compactMode); await refresh(); notify("success", "设置与工具链已保存"); } catch (error) { notify("error", "工具链配置无效", error instanceof Error ? error.message : String(error)); } })(); }} />;

  return (
    <div className={`app-shell ${collapsed ? "sidebar-collapsed" : ""} ${settings.compactMode ? "compact" : ""}`}>
      <header className="titlebar" data-tauri-drag-region>
        {macPlatform && <div className="mac-window-controls">
          <button className="mac-close" aria-label="关闭窗口" onClick={() => void windowAction("close")}><X size={8} /></button>
          <button className="mac-minimize" aria-label="最小化窗口" onClick={() => void windowAction("minimize")}><Minus size={8} /></button>
          <button className="mac-maximize" aria-label="最大化窗口" onClick={() => void windowAction("maximize")}><Square size={7} /></button>
        </div>}
        <div className="titlebar-brand"><img src="/brand/mobius-mark.png" alt="" /><span>Mobius</span><small>DEVICE LAB</small></div>
        <div className="titlebar-center"><button className="command-trigger" aria-haspopup="dialog" onClick={() => setPaletteOpen(true)}><Search size={14} /><span>搜索设备、操作或设置</span><kbd>{commandKey()} K</kbd></button></div>
        <div className="titlebar-actions"><button className="icon-button" aria-label="打开活动记录" title="活动记录" onClick={() => setUtilityModal("activity")}><Activity size={16} /></button><button className="icon-button" aria-label="打开帮助" title="帮助与快捷键" onClick={() => setUtilityModal("help")}><HelpCircle size={16} /></button>{!macPlatform && <div className="window-controls"><button aria-label="最小化窗口" onClick={() => void windowAction("minimize")}><Minus size={14} /></button><button aria-label="最大化窗口" onClick={() => void windowAction("maximize")}><Square size={11} /></button><button className="window-close" aria-label="关闭窗口" onClick={() => void windowAction("close")}><X size={14} /></button></div>}</div>
      </header>

      <aside className="sidebar">
        <nav>{navigation.filter((item) => item.id !== "settings").map(({ id, label, icon: Icon, shortcut }) => <button key={id} aria-label={label} aria-current={page === id ? "page" : undefined} className={page === id ? "active" : ""} onClick={() => navigate(id)} title={collapsed ? label : undefined}><Icon size={18} /><span>{label}</span><kbd>{shortcut}</kbd></button>)}</nav>
        <div className="sidebar-bottom">
          <button aria-label="设置" aria-current={page === "settings" ? "page" : undefined} className={page === "settings" ? "active" : ""} onClick={() => navigate("settings")}><SettingsIcon size={18} /><span>设置</span><kbd>6</kbd></button>
          <button aria-label={collapsed ? "展开侧栏" : "收起侧栏"} onClick={() => setCollapsed((value) => !value)}><PanelLeftClose size={18} /><span>收起侧栏</span></button>
        </div>
      </aside>

      <section className="device-context-bar">
        <button className="sidebar-toggle" onClick={() => setCollapsed((value) => !value)}><Menu size={17} /></button>
        {activeDevice ? <>
          <div className="device-picker-shell" ref={devicePickerRef}>
            <button
              ref={devicePickerTriggerRef}
              type="button"
              className={`device-selector ${devicePickerOpen ? "open" : ""}`}
              aria-label={`切换当前设备，当前为 ${activeDevice.name}`}
              aria-haspopup="menu"
              aria-expanded={devicePickerOpen}
              aria-controls="device-picker-menu"
              onClick={() => devicePickerOpen ? closeDevicePicker() : openDevicePicker()}
              onKeyDown={devicePickerTriggerKey}
            >
              <DeviceIdentity device={activeDevice} compact />
              <ChevronDown size={15} />
            </button>
            {devicePickerOpen && <div id="device-picker-menu" className="device-picker-menu" role="menu" aria-label="选择当前设备" onKeyDown={devicePickerMenuKey}>
              <div className="device-picker-heading" role="none"><strong>切换当前设备</strong><span>选择后留在当前页面</span></div>
              <div className="device-picker-list" role="none">
                {pickerDevices.map((device, index) => {
                  const isSelected = device.id === activeDevice.id;
                  const stateLabel = device.state === "online" ? "已连接" : device.state === "connecting" ? "连接中" : device.state === "registered" ? "已登记 · 待验证" : device.state === "unauthorized" ? "待授权" : "离线";
                  const locator = device.address ?? (device.transport === "usb" || device.transport === "usbmux" ? "USB 直连" : device.transport === "emulator" ? "本机模拟器" : "网络设备");
                  return <button
                    key={device.id}
                    type="button"
                    role="menuitemradio"
                    aria-checked={isSelected}
                    aria-label={`${device.name}，${device.platform === "android" ? "Android" : "iOS"}，${stateLabel}，${locator}`}
                    className={isSelected ? "selected" : ""}
                    data-device-index={index}
                    tabIndex={index === devicePickerIndex ? 0 : -1}
                    onFocus={() => setDevicePickerIndex(index)}
                    onClick={() => selectDeviceFromPicker(device)}
                  >
                    <span className="device-picker-identity"><DeviceIdentity device={device} compact /><small>{device.platform === "android" ? "Android" : "iOS"} · {locator}</small></span>
                    <span className="device-picker-state"><span><StatusDot status={device.state === "online" ? "success" : device.state === "connecting" ? "running" : device.state === "registered" || device.state === "unauthorized" ? "warning" : "muted"} />{stateLabel}</span><i aria-hidden="true">{isSelected ? "✓" : ""}</i></span>
                  </button>;
                })}
              </div>
              <div className="device-picker-footer" role="none"><button type="button" role="menuitem" onClick={() => page === "devices" ? openDevices("legacy") : openDevices("list")}><Cable size={14} /><span>{page === "devices" ? "添加 / 连接设备" : "返回设备工作台"}</span></button></div>
            </div>}
          </div>
          <div className="context-divider" />
          <div className="context-state"><StatusDot status={activeDevice.state === "online" ? "success" : "warning"} /><span>{activeDevice.state === "online" ? "已连接" : activeDevice.state === "registered" ? "已登记 · 待验证" : activeDevice.state}</span></div>
          <StatusBadge tone={activeDevice.rooted || activeDevice.jailbroken || activeIosSshSession?.jailbreakConfirmed ? "purple" : "neutral"}>{activeDevice.rooted ? "ROOT" : activeDevice.jailbroken || activeIosSshSession?.jailbreakConfirmed ? "JAILBROKEN" : activeDevice.platform === "ios" ? activeIosSshSession?.connected ? "SSH" : "IOS" : "SHELL"}</StatusBadge>
          <div className="context-actions">{activeDevice.platform === "android" ? <><Button variant="ghost" icon={<Wifi size={14} />} disabled={activeDevice.state !== "online"} onClick={() => setModal("proxy")}>代理</Button>{page !== "devices" && <Button variant="ghost" icon={<MonitorSmartphone size={14} />} disabled={activeDevice.state !== "online"} onClick={() => openDevices("list")}>屏幕</Button>}<Button variant="ghost" icon={<TerminalSquare size={14} />} disabled={activeDevice.state !== "online"} onClick={() => openDebug("shell")}>Shell</Button></> : <><Button variant="ghost" icon={<FolderSync size={14} />} onClick={() => navigate("files")}>{activeIosSshSession ? "文件" : "连接 SSH"}</Button><Button variant="ghost" icon={<Wrench size={14} />} onClick={() => openDebug("system")}>工具</Button></>}<Button variant="ghost" icon={<KeyRound size={14} />} disabled={activeDevice.platform === "android" ? activeDevice.state !== "online" : !activeIosSshSession?.jailbreakConfirmed} onClick={() => activeDevice.platform === "ios" ? openDebug("instrumentation") : setModal("frida")}>Frida</Button></div>
        </> : <><div className="no-device"><StatusDot status="muted" /><span>没有已选择的设备</span></div><Button variant="primary" icon={<Cable size={14} />} onClick={() => openDevices("legacy")}>连接设备</Button></>}
      </section>

      <main ref={mainContentRef} className="main-content">{content}</main>

      <footer className="statusbar">
        <button className="statusbar-health" title={toolHealthTitle} onClick={() => { setSettingsGroup("toolchain"); navigate("settings"); }}><Wrench size={12} /><StatusDot status={toolHealthStatus} /><span>{toolHealthLabel}</span></button>
        {activeDevice?.platform === "ios" ? <div><StatusDot status={activeIosSshSession?.connected ? "success" : "warning"} /><span>{activeIosSshSession?.connected ? `SSH ${activeIosSshSession.sshHost}:${activeIosSshSession.sshPort}` : "iOS SSH 待连接"}</span></div> : activeDevice ? <div><StatusDot status={activeDevice.state === "online" ? "success" : "warning"} /><span>{activeDevice.state === "online" ? "ADB 已连接" : "ADB 待连接"}</span></div> : null}
        <div><Smartphone size={12} /><span>{presentedDevices.filter((device) => device.state === "online").length} 台在线</span></div>
        <button className="statusbar-tasks" onClick={() => setUtilityModal("activity")}><Activity size={12} /><span>{activities.length} 条活动</span></button>
      </footer>

      {modal === "scrcpy" && activeDevice && <ScrcpyDialog device={activeDevice} onClose={() => setModal(null)} notify={notify} record={record} />}
      {modal === "frida" && activeDevice && <FridaDialog device={activeDevice} result={activeAndroidFridaResult} onResultChange={(result) => setAndroidFridaResults((current) => { const next = { ...current }; if (result) next[activeDevice.id] = result; else delete next[activeDevice.id]; return next; })} onClose={() => setModal(null)} notify={notify} record={record} />}
      {modal === "proxy" && activeDevice && <QuickProxyDialog device={activeDevice} defaults={{ host: settings.proxyHost, port: settings.proxyPort }} onSaveDefaults={(host, port) => { const next = { ...settings, proxyHost: host, proxyPort: port }; setSettings(next); localStorage.setItem("mobius.settings", JSON.stringify(next)); }} onClose={() => setModal(null)} notify={notify} record={record} />}
      {utilityModal === "activity" && <Modal title="活动记录" subtitle="本次 Mobius 会话中的操作与错误" width={680} onClose={() => setUtilityModal(null)} footer={<><Button icon={<Trash2 size={14} />} disabled={!activities.length} onClick={() => setActivities([])}>清空记录</Button><Button variant="primary" onClick={() => setUtilityModal(null)}>完成</Button></>}>
        {activities.length ? <div className="utility-activity-list">{activities.map((item) => <div key={item.id}><StatusDot status={item.status} /><span><strong>{item.title}</strong><small>{item.detail}</small></span><time>{item.at}</time></div>)}</div> : <div className="utility-empty"><Activity size={24} /><strong>还没有操作记录</strong><span>连接、传输、代理、截图和调试操作会显示在这里。</span></div>}
      </Modal>}
      {utilityModal === "help" && <Modal title="快捷使用" subtitle="Mobius Device Lab 0.1 Preview" width={660} onClose={() => setUtilityModal(null)} footer={<Button variant="primary" onClick={() => setUtilityModal(null)}>知道了</Button>}>
        <div className="help-grid">
          <section><h3>全局快捷键</h3><div><kbd>{commandKey()} K</kbd><span>搜索设备与常用操作</span></div>{navigation.map((item) => <div key={item.id}><kbd>{commandKey()} {item.shortcut}</kbd><span>打开{item.label}</span></div>)}</section>
          <section><h3>最短路径</h3><div><kbd>工作台</kbd><span>投屏、连接、截图和录屏</span></div><div><kbd>应用</kbd><span>拖入 APK / IPA 后解析与安装</span></div><div><kbd>文件</kbd><span>Android ADB 或 iOS SSH 管理</span></div><div><kbd>网络</kbd><span>Reverse 与系统代理分别控制</span></div><div><kbd>调试</kbd><span>系统结果、Shell 与 Frida</span></div></section>
        </div>
        <InlineNotice tone="info" title="设备始终固定">顶部设备菜单可在当前页面原地切换；每个写操作仍会显示并绑定明确目标。</InlineNotice>
      </Modal>}
      {paletteOpen && <div className="palette-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && setPaletteOpen(false)}><section className="command-palette" role="dialog" aria-modal="true" aria-label="命令面板"><header><Command size={18} /><input autoFocus value={paletteQuery} onChange={(e) => setPaletteQuery(e.target.value)} onKeyDown={paletteKey} placeholder="输入设备、操作或页面名称…" aria-controls="command-results" aria-activedescendant={paletteItems[paletteIndex] ? `command-${paletteItems[paletteIndex].id}` : undefined} /><kbd>ESC</kbd></header><div id="command-results" className="palette-results" role="listbox">{paletteItems.map(({ id, label, detail, icon: Icon, run }, index) => <button id={`command-${id}`} role="option" aria-selected={index === paletteIndex} key={id} className={index === paletteIndex ? "active" : ""} onMouseEnter={() => setPaletteIndex(index)} onClick={() => { void run(); setPaletteOpen(false); }}><span><Icon size={17} /></span><div><strong>{label}</strong><small>{detail}</small></div><Play size={13} /></button>)}{!paletteItems.length && <div className="palette-empty">没有匹配的设备或操作</div>}</div><footer><span><kbd>↑</kbd><kbd>↓</kbd> 选择</span><span><kbd>↵</kbd> 打开</span><span><Sparkles size={12} /> Mobius Command</span></footer></section></div>}
      <ToastStack messages={toasts} dismiss={(id) => setToasts((current) => current.filter((item) => item.id !== id))} />
    </div>
  );
}

function ScrcpyDialog({ device, onClose, notify, record }: { device: Device; onClose: () => void; notify: (type: ToastMessage["type"], title: string, detail?: string) => void; record: (title: string, detail: string, status?: ActivityItem["status"]) => void }) {
  const [maxSize, setMaxSize] = useState("1920");
  const [bitRate, setBitRate] = useState("8M");
  const [stayAwake, setStayAwake] = useState(true);
  const [turnScreenOff, setTurnScreenOff] = useState(false);
  const [busy, setBusy] = useState(false);
  const launch = async () => {
    setBusy(true);
    try {
      const result = await api.launchScrcpy(device.id, { maxSize: Number(maxSize), bitRate, stayAwake, turnScreenOff });
      if (!result.success) throw new Error(result.message);
      notify("success", "投屏窗口已启动", result.pid ? `进程 PID ${result.pid}` : result.message);
      record("启动 scrcpy", `${device.name} · 最大 ${maxSize}px · ${bitRate}`);
      onClose();
    } catch (error) { notify("error", "无法启动 scrcpy", error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  };
  return <Modal title="启动 Android 投屏" subtitle={`目标：${device.name} · ${device.id}`} onClose={onClose} footer={<><Button onClick={onClose}>取消</Button><Button variant="primary" icon={busy ? <LoaderCircle className="spin" size={14} /> : <MonitorSmartphone size={14} />} disabled={busy || device.platform !== "android"} onClick={launch}>启动独立窗口</Button></>}>
    <div className="form-stack"><InlineNotice tone="info" title="内嵌实时画面是默认主路径">设备页会直接显示 scrcpy 实时视频；这个次级入口只用于弹出原生低延迟交互窗口，适合需要鼠标、键盘操控的场景。</InlineNotice><div className="field-row"><Field label="最大边长"><select value={maxSize} onChange={(e) => setMaxSize(e.target.value)}><option value="1280">1280 px</option><option value="1920">1920 px</option><option value="2560">2560 px</option><option value="0">设备原始分辨率</option></select></Field><Field label="视频码率"><select value={bitRate} onChange={(e) => setBitRate(e.target.value)}><option>4M</option><option>8M</option><option>12M</option><option>16M</option></select></Field></div><div className="toggle-list compact-toggles"><label><div><strong>保持唤醒</strong><span>投屏期间避免设备休眠</span></div><input type="checkbox" checked={stayAwake} onChange={(e) => setStayAwake(e.target.checked)} /><i /></label><label><div><strong>关闭设备屏幕</strong><span>仅保留电脑端画面</span></div><input type="checkbox" checked={turnScreenOff} onChange={(e) => setTurnScreenOff(e.target.checked)} /><i /></label></div><div className="command-preview"><span>命令预览</span><code>scrcpy --serial {device.id} --max-size {maxSize} --video-bit-rate {bitRate}{stayAwake ? " --stay-awake" : ""}{turnScreenOff ? " --turn-screen-off" : ""}</code></div></div>
  </Modal>;
}

function QuickProxyDialog({ device, defaults, onSaveDefaults, onClose, notify, record }: { device: Device; defaults: { host: string; port: string }; onSaveDefaults: (host: string, port: string) => void; onClose: () => void; notify: (type: ToastMessage["type"], title: string, detail?: string) => void; record: (title: string, detail: string, status?: ActivityItem["status"]) => void }) {
  const [mode, setMode] = useState<"reverse" | "lan">(defaults.host === "127.0.0.1" ? "reverse" : "lan");
  const [host, setHost] = useState(defaults.host === "127.0.0.1" ? "" : defaults.host);
  const [port, setPort] = useState(defaults.port || "8080");
  const [setSystemProxy, setSetSystemProxy] = useState(false);
  const [busy, setBusy] = useState(false);
  const apply = async () => {
    const parsedPort = Number(port);
    if (!parsedPort || parsedPort > 65535) { notify("warning", "请输入有效代理端口"); return; }
    if (mode === "lan" && !host.trim()) { notify("warning", "请输入测试主机地址"); return; }
    setBusy(true);
    let reverseCreated = false;
    try {
      if (mode === "reverse") {
        const mapping = { serial: device.id, direction: "reverse" as const, local: `tcp:${parsedPort}`, remote: `tcp:${parsedPort}` };
        const existing = await api.mappings(device.id).catch(() => []);
        const alreadyMapped = existing.some((item) => item.direction === "reverse" && item.local === mapping.local && item.remote === mapping.remote);
        if (!alreadyMapped) {
          const result = await api.createMapping(mapping);
          if (!result.success) throw new Error(result.message);
          reverseCreated = true;
        }
      }
      if (mode === "reverse" && !setSystemProxy) {
        onSaveDefaults("127.0.0.1", String(parsedPort));
        notify("success", "USB Reverse 已创建", `设备 127.0.0.1:${parsedPort} → 本机 ${parsedPort}；未修改系统代理。`);
        record("创建代理 Reverse", `${device.name} · tcp:${parsedPort} · 未设置系统代理`);
        onClose();
        return;
      }
      const proxyHost = mode === "reverse" ? "127.0.0.1" : host.trim();
      const result = await api.setProxy(device.id, proxyHost, parsedPort);
      if (!result.success) throw new Error(result.message);
      onSaveDefaults(proxyHost, String(parsedPort));
      notify("success", "测试代理已就绪", mode === "reverse" ? `设备 127.0.0.1:${parsedPort} → 本机 Burp ${parsedPort}` : `${proxyHost}:${parsedPort}`);
      record("设置 Android 测试代理", `${device.name} · ${mode === "reverse" ? "USB Reverse" : proxyHost}:${parsedPort}`);
      onClose();
    } catch (error) {
      if (reverseCreated) await api.removeMapping({ serial: device.id, direction: "reverse", local: `tcp:${parsedPort}`, remote: `tcp:${parsedPort}` }).catch(() => undefined);
      notify("error", "代理设置失败", error instanceof Error ? error.message : String(error));
    } finally { setBusy(false); }
  };
  const clear = async () => {
    setBusy(true);
    try {
      const result = await api.clearProxy(device.id);
      if (!result.success) throw new Error(result.message);
      notify("success", "设备系统代理已恢复", "USB Reverse 保持不变，可单独在“网络”页面清理。");
      record("恢复 Android 系统代理", device.name);
      onClose();
    } catch (error) { notify("error", "代理恢复失败", error instanceof Error ? error.message : String(error)); }
    finally { setBusy(false); }
  };
  return <Modal title="快速连接测试代理" subtitle={`目标：${device.name} · ${device.id}`} onClose={onClose} width={580} footer={<><Button disabled={busy} onClick={clear}>恢复系统代理</Button><Button onClick={onClose}>取消</Button><Button variant="primary" icon={busy ? <LoaderCircle className="spin" size={14} /> : <Wifi size={14} />} disabled={busy || device.platform !== "android" || device.state !== "online"} onClick={apply}>{mode === "reverse" ? setSystemProxy ? "创建 Reverse 并设置代理" : "仅创建 Reverse" : "设置系统代理"}</Button></>}>
    <div className="form-stack">
      <div className="proxy-mode-picker"><button className={mode === "reverse" ? "active" : ""} onClick={() => setMode("reverse")}><Cable size={18} /><span><strong>USB 反向隧道</strong><small>推荐 · 无需查找电脑 IP</small></span></button><button className={mode === "lan" ? "active" : ""} onClick={() => setMode("lan")}><Network size={18} /><span><strong>局域网直连</strong><small>设备直接访问测试主机</small></span></button></div>
      <InlineNotice tone="info" title={mode === "reverse" ? "隧道与系统代理独立" : "局域网模式"}>{mode === "reverse" ? "默认只创建 ADB Reverse，适合 Reqable 等自行设置代理的工具；需要 Burp 全局代理时勾选下方选项。" : "确保设备能访问该主机地址，且代理监听范围没有暴露到不受信网络。"}</InlineNotice>
      <div className="field-row">{mode === "lan" && <Field label="测试主机 IP"><input autoFocus value={host} onChange={(event) => setHost(event.target.value)} placeholder="192.168.1.2" /></Field>}<Field label="Burp 监听端口"><input autoFocus={mode === "reverse"} value={port} onChange={(event) => setPort(event.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></Field></div>
      {mode === "reverse" && <label className="check-row"><input type="checkbox" checked={setSystemProxy} onChange={(event) => setSetSystemProxy(event.target.checked)} /><span>同时设置 Android 系统代理为 127.0.0.1:{port || "…"}</span></label>}
      <div className="command-preview"><span>操作预览</span><code>{mode === "reverse" ? `adb -s ${device.id} reverse tcp:${port || "…"} tcp:${port || "…"}${setSystemProxy ? `\nadb -s ${device.id} shell settings put global http_proxy 127.0.0.1:${port || "…"}` : "\n# 不修改 Android 系统代理"}` : `adb -s ${device.id} shell settings put global http_proxy ${host || "<host>"}:${port || "…"}`}</code></div>
    </div>
  </Modal>;
}

function FridaDialog({ device, result, onResultChange, onClose, notify, record }: { device: Device; result?: FridaServerResult; onResultChange: (result?: FridaServerResult) => void; onClose: () => void; notify: (type: ToastMessage["type"], title: string, detail?: string) => void; record: (title: string, detail: string, status?: ActivityItem["status"]) => void }) {
  const [profile, setProfile] = useState<"16.1.4" | "17.17.0" | "custom">("16.1.4");
  const [customVersion, setCustomVersion] = useState("");
  const [path, setPath] = useState("");
  const [devicePort, setDevicePort] = useState("27042");
  const [hostPort, setHostPort] = useState("27042");
  const [confirmed, setConfirmed] = useState(false);
  const [busy, setBusy] = useState<"start" | "stop">();
  const canServer = device.platform === "android" && device.state === "online" && !!device.rooted;
  const active = !!result?.active;
  useEffect(() => {
    const key = `mobius.frida.${profile}.${device.architecture ?? "unknown"}`;
    setPath(localStorage.getItem(key) ?? "");
  }, [profile, device.architecture]);
  const chooseServer = async () => {
    const selected = await chooseBinaryFile(`选择 Frida Server ${profile === "custom" ? customVersion || "自定义版本" : profile} · ${device.architecture ?? "匹配设备架构"}`);
    if (selected) setPath(selected);
  };
  const start = async () => {
    const parsedDevicePort = Number(devicePort);
    const parsedHostPort = Number(hostPort);
    if (!parsedDevicePort || parsedDevicePort > 65535 || !parsedHostPort || parsedHostPort > 65535) { notify("warning", "请输入有效的设备端口与主机端口"); return; }
    setBusy("start");
    try {
      const result = await api.startFrida(device.id, device.platform, path.trim(), parsedDevicePort, parsedHostPort);
      if (!result.success) throw new Error(result.message);
      onResultChange(result);
      localStorage.setItem(`mobius.frida.${profile}.${device.architecture ?? "unknown"}`, path.trim());
      notify("success", "Frida Server 已启动", `PID ${result.pid ?? "—"} · ${result.remotePath} · 127.0.0.1:${result.hostPort ?? parsedHostPort}`);
      record("启动 Frida Server", `${device.name} · ${result.remotePath} · 主机 ${result.hostPort ?? parsedHostPort} → 设备 ${result.devicePort ?? parsedDevicePort}`);
      onClose();
    } catch (error) { notify("error", "Frida 启动失败", error instanceof Error ? error.message : String(error)); }
    finally { setBusy(undefined); }
  };
  const stop = async () => {
    if (!active || busy) return;
    setBusy("stop");
    try {
      const stopped = await api.stopFrida(device.id);
      if (!stopped.success) throw new Error(stopped.message);
      onResultChange(undefined);
      notify("success", "Frida Server 已停止", `已清理 ${device.name} 的托管进程与 ADB Forward。`);
      record("停止 Frida Server", `${device.name} · ${stopped.remotePath}`);
      onClose();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "Frida 停止失败", message);
      record("Frida 停止失败", `${device.name} · ${message}`, "error");
    } finally { setBusy(undefined); }
  };
  const selectedVersion = profile === "custom" ? customVersion || "自定义" : profile;
  return <Modal title={active ? "Frida Server 正在运行" : "启动 Frida Server"} subtitle={`目标：${device.name} · ${device.architecture ?? "架构待检测"}`} onClose={onClose} width={680} footer={active ? <><Button onClick={onClose}>关闭</Button><Button variant="danger" icon={busy === "stop" ? <LoaderCircle className="spin" size={14} /> : <CircleStop size={14} />} disabled={!!busy} onClick={stop}>停止并清理转发</Button></> : <><Button onClick={onClose}>取消</Button><Button variant="primary" icon={busy === "start" ? <LoaderCircle className="spin" size={14} /> : <Play size={14} />} disabled={!canServer || !path.trim() || !confirmed || !!busy || (profile === "custom" && !customVersion.trim())} onClick={start}>上传、启动并转发</Button></>}>
    <div className="form-stack">{active && result ? <>
      <InlineNotice tone="success" title="托管会话已就绪">PID {result.pid ?? "—"} · 本机 {result.listenAddress ?? "127.0.0.1"}:{result.hostPort ?? "—"} → 设备 127.0.0.1:{result.devicePort ?? "—"}</InlineNotice>
      <div className="operation-plan"><h3>当前资源</h3><ol><li><span>✓</span><div><strong>{result.remotePath}</strong><small>设备端中性路径</small></div></li><li><span>✓</span><div><strong>PID {result.pid ?? "—"}</strong><small>仅停止本次 Mobius 记录并复核身份的进程</small></div></li><li><span>✓</span><div><strong>{result.mapping?.local ?? `tcp:${result.hostPort ?? "—"}`} → {result.mapping?.remote ?? `tcp:${result.devicePort ?? "—"}`}</strong><small>ADB Forward 仅监听本机</small></div></li></ol></div>
    </> : <>{canServer ? <InlineNotice tone="warning" title="写设备操作">Mobius 会上传你明确选择的 Server、使用中性远端别名、启动后核验进程，并创建仅绑定本机的 ADB Forward。</InlineNotice> : <InlineNotice tone="warning" title={device.platform === "ios" ? "请在调试页使用 iOS 工作流" : "需要在线 Root Android"}>{device.platform === "ios" ? "iOS Frida 已通过经验证的 SSH 会话托管；请在调试页上传 Mach-O Server 并启动。" : "非 Root Android 应使用 Gadget 或可调试 App 工作流；Mobius 不会显示假成功。"}</InlineNotice>}
      <div className="frida-version-picker"><button className={profile === "16.1.4" ? "active" : ""} onClick={() => setProfile("16.1.4")}><strong>16.1.4</strong><small>默认兼容</small></button><button className={profile === "17.17.0" ? "active" : ""} onClick={() => setProfile("17.17.0")}><strong>17.17.0</strong><small>最新稳定</small></button><button className={profile === "custom" ? "active" : ""} onClick={() => setProfile("custom")}><strong>自定义</strong><small>其他版本</small></button></div>
      {profile === "custom" && <Field label="版本标识"><input value={customVersion} onChange={(event) => setCustomVersion(event.target.value.replace(/[^0-9A-Za-z._-]/g, "").slice(0, 40))} placeholder="例如 17.16.2" disabled={!canServer} /></Field>}
      <Field label={`Frida Server ${selectedVersion} · ${device.architecture ?? "匹配设备架构"}`} hint="选择已解压、文件名以 frida-server 开头且 ABI 匹配的文件；Mobius 不下载或捆绑 Server。"><div className="path-input"><input readOnly value={path} placeholder="选择本机 Server 文件" title={path} disabled={!canServer} /><button type="button" disabled={!canServer} onClick={chooseServer}>选择</button></div></Field>
      <div className="field-row"><Field label="设备监听端口"><input value={devicePort} onChange={(event) => setDevicePort(event.target.value.replace(/\D/g, "").slice(0, 5))} disabled={!canServer} inputMode="numeric" /></Field><Field label="主机转发端口"><input value={hostPort} onChange={(event) => setHostPort(event.target.value.replace(/\D/g, "").slice(0, 5))} disabled={!canServer} inputMode="numeric" /></Field></div>
      <div className="operation-plan"><h3>执行计划</h3><ol><li><span>1</span><div><strong>校验目标、架构和文件</strong><small>{device.architecture ?? "运行时检测"} · 配置 {selectedVersion}</small></div></li><li><span>2</span><div><strong>上传为中性且可审计的名称</strong><small>/data/local/tmp/mobius-agentd（不修改二进制内容）</small></div></li><li><span>3</span><div><strong>启动并记录进程身份</strong><small>保存 PID、真实路径与 Linux 启动时间</small></div></li><li><span>4</span><div><strong>自动创建回环转发</strong><small>本机 127.0.0.1:{hostPort || "…"} → 设备 127.0.0.1:{devicePort || "…"}</small></div></li></ol></div>
      <label className="check-row"><input type="checkbox" checked={confirmed} onChange={(e) => setConfirmed(e.target.checked)} disabled={!canServer} /><span>我确认此设备归我所有或已获得明确测试授权，并已核对 Server 版本与 ABI</span></label>
    </>}</div>
  </Modal>;
}
