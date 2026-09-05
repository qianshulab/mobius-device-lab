import { ArrowDownToLine, ArrowLeft, ArrowRight, ArrowUpFromLine, Cable, ChevronRight, CircleStop, File, FileArchive, FileCode2, Folder, FolderLock, FolderOpen, HardDrive, Home, KeyRound, Link2, LoaderCircle, Network as NetworkIcon, Plus, RefreshCw, Save, Search, Settings2, ShieldAlert, ShieldCheck, Trash2 } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../lib/api";
import { chooseDirectory, chooseLocalFile } from "../lib/dialog";
import { formatBytes } from "../lib/format";
import type { Device, IosSshSession, RemoteFile } from "../types";
import { Button, EmptyState, Field, InlineNotice, Modal, Panel, StatusBadge, Tabs } from "../components/Ui";

interface FilesProps {
  activeDevice?: Device;
  iosSession?: IosSshSession;
  onIosSessionChange: (session?: IosSshSession) => void;
  notify: (type: "success" | "error" | "info" | "warning", title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: "success" | "warning" | "error" | "running" | "info") => void;
}

interface IosSshPreset {
  mode: "usb" | "lan";
  authMode: "password" | "privateKey";
  host: string;
  devicePort: string;
  hostPort: string;
  username: string;
  privateKeyPath: string;
  allowedRoots: string;
}

const iosSshPresetStorageKey = "mobius.files.ios-ssh-presets.v1";
const autoAttemptedIosDevices = new Set<string>();
const filesPathByDevice = new Map<string, string>();

function defaultIosSshPreset(device?: Device): IosSshPreset {
  const useLan = device?.platform === "ios" && device.transport === "wifi";
  const endpoint = useLan ? parseSshEndpoint(device.address) : undefined;
  return {
    mode: useLan ? "lan" : "usb",
    authMode: "password",
    host: endpoint?.host ?? "",
    devicePort: endpoint?.port ?? "22",
    hostPort: "",
    username: "root",
    privateKeyPath: "",
    allowedRoots: "/var/mobile",
  };
}

function readIosSshPreset(device?: Device): { preset: IosSshPreset; saved: boolean } {
  const fallback = defaultIosSshPreset(device);
  if (!device || typeof localStorage === "undefined") return { preset: fallback, saved: false };
  try {
    const stored = JSON.parse(localStorage.getItem(iosSshPresetStorageKey) ?? "{}");
    const candidate = stored?.[device.id];
    if (!candidate || typeof candidate !== "object") return { preset: fallback, saved: false };
    const mode = candidate.mode === "lan" || candidate.mode === "usb" ? candidate.mode : fallback.mode;
    const authMode = candidate.authMode === "privateKey" ? "privateKey" : "password";
    return {
      saved: true,
      preset: {
        mode,
        authMode,
        host: typeof candidate.host === "string" ? candidate.host : fallback.host,
        devicePort: typeof candidate.devicePort === "string" && /^\d{1,5}$/.test(candidate.devicePort) ? candidate.devicePort : fallback.devicePort,
        hostPort: typeof candidate.hostPort === "string" && /^\d{0,5}$/.test(candidate.hostPort) ? candidate.hostPort : "",
        username: typeof candidate.username === "string" && candidate.username.trim() ? candidate.username : "root",
        privateKeyPath: typeof candidate.privateKeyPath === "string" ? candidate.privateKeyPath : "",
        allowedRoots: typeof candidate.allowedRoots === "string" && candidate.allowedRoots.trim() ? candidate.allowedRoots : "/var/mobile",
      },
    };
  } catch {
    return { preset: fallback, saved: false };
  }
}

function writeIosSshPreset(deviceId: string, preset: IosSshPreset) {
  if (typeof localStorage === "undefined") return;
  let stored: Record<string, IosSshPreset> = {};
  try {
    const parsed = JSON.parse(localStorage.getItem(iosSshPresetStorageKey) ?? "{}");
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) stored = parsed;
  } catch {
    // Replace malformed local-only preferences with the current verified shape.
  }
  localStorage.setItem(iosSshPresetStorageKey, JSON.stringify({ ...stored, [deviceId]: preset }));
}

function normalizeRemotePath(value: string) {
  const parts: string[] = [];
  for (const part of value.trim().split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") parts.pop();
    else parts.push(part);
  }
  return `/${parts.join("/")}`;
}

function isWithinRoot(candidate: string, root: string) {
  return candidate === root || candidate.startsWith(`${root}/`);
}

function logicalRemoteChildPath(parent: string, name: string) {
  const normalizedParent = normalizeRemotePath(parent);
  return normalizedParent === "/" ? `/${name}` : `${normalizedParent}/${name}`;
}

function parseSshEndpoint(address?: string) {
  if (!address) return undefined;
  const separator = address.lastIndexOf(":");
  if (separator < 1) return { host: address, port: "22" };
  const host = address.slice(0, separator);
  const port = address.slice(separator + 1);
  return { host, port: /^\d{1,5}$/.test(port) ? port : "22" };
}

function fileIcon(entry: RemoteFile) {
  if (entry.kind === "directory") return <Folder size={17} />;
  if (entry.kind === "link") return <Link2 size={17} />;
  if (/\.(apk|ipa|zip|xz|gz)$/i.test(entry.name)) return <FileArchive size={17} />;
  if (/\.(js|json|xml|plist|txt|log)$/i.test(entry.name)) return <FileCode2 size={17} />;
  return <File size={17} />;
}

export default function FilesPage({ activeDevice, iosSession, onIosSessionChange, notify, record }: FilesProps) {
  const initialPath = activeDevice?.platform === "ios" ? "/var/mobile" : "/sdcard";
  const [path, setPath] = useState(initialPath);
  const [pathInput, setPathInput] = useState(initialPath);
  const [entries, setEntries] = useState<RemoteFile[]>([]);
  const [fileError, setFileError] = useState<string>();
  const [selected, setSelected] = useState<string>();
  const [loading, setLoading] = useState(false);
  const [search, setSearch] = useState("");
  const [transfer, setTransfer] = useState<"upload" | "download" | null>(null);
  const [localPath, setLocalPath] = useState("");
  const [remotePath, setRemotePath] = useState("");
  const [overwrite, setOverwrite] = useState(false);
  const [newFolder, setNewFolder] = useState(false);
  const [folderName, setFolderName] = useState("");
  const [deleteTarget, setDeleteTarget] = useState<RemoteFile>();
  const [iosMode, setIosMode] = useState<"usb" | "lan">("usb");
  const [iosAuthMode, setIosAuthMode] = useState<"password" | "privateKey">("password");
  const [iosHost, setIosHost] = useState("");
  const [iosDevicePort, setIosDevicePort] = useState("22");
  const [iosHostPort, setIosHostPort] = useState("");
  const [iosUsername, setIosUsername] = useState("root");
  const [iosPassword, setIosPassword] = useState("alpine");
  const [iosPrivateKey, setIosPrivateKey] = useState("");
  const [iosAllowedRoots, setIosAllowedRoots] = useState("/var/mobile");
  const [iosConnecting, setIosConnecting] = useState(false);
  const [iosSettingsOpen, setIosSettingsOpen] = useState(false);
  const [iosPresetSaved, setIosPresetSaved] = useState(false);
  const [selectedIosRoot, setSelectedIosRoot] = useState("/var/mobile");
  const contextRef = useRef("");
  const loadRequestRef = useRef(0);
  const [iosPresetReadyFor, setIosPresetReadyFor] = useState("");

  const contextKey = `${activeDevice?.id ?? ""}\u0000${iosSession?.sessionId ?? ""}`;
  contextRef.current = contextKey;
  const contextIsCurrent = useCallback(
    (deviceId: string, sessionId?: string) => contextRef.current === `${deviceId}\u0000${sessionId ?? ""}`,
    [],
  );

  const iosAllowedRootsKey = (iosSession?.allowedRoots ?? []).join("\u0001");
  const allowedIosRoots = useMemo(
    () => [...new Set(iosAllowedRootsKey ? iosAllowedRootsKey.split("\u0001").map(normalizeRemotePath) : [])],
    [iosAllowedRootsKey],
  );
  const currentIosRoot = allowedIosRoots.includes(selectedIosRoot) ? selectedIosRoot : allowedIosRoots[0] ?? "/var/mobile";

  useEffect(() => {
    setIosPresetReadyFor("");
    if (activeDevice?.platform !== "ios") return;
    const { preset, saved } = readIosSshPreset(activeDevice);
    setIosMode(preset.mode);
    setIosAuthMode(preset.authMode);
    setIosHost(preset.host);
    setIosDevicePort(preset.devicePort);
    setIosHostPort(preset.hostPort);
    setIosUsername(preset.username);
    setIosPassword("alpine");
    setIosPrivateKey(preset.privateKeyPath);
    setIosAllowedRoots(preset.allowedRoots);
    setIosPresetSaved(saved);
    setIosSettingsOpen(false);
    setIosPresetReadyFor(activeDevice.id);
  }, [activeDevice?.address, activeDevice?.id, activeDevice?.platform, activeDevice?.transport]);

  useEffect(() => {
    loadRequestRef.current += 1;
    setEntries([]);
    setFileError(undefined);
    setSelected(undefined);
    setSearch("");
    setTransfer(null);
    setLocalPath("");
    setRemotePath("");
    setOverwrite(false);
    setNewFolder(false);
    setFolderName("");
    setDeleteTarget(undefined);
    setLoading(false);
    setIosPassword("alpine");
  }, [activeDevice?.id, iosSession?.sessionId]);

  const loadDeviceId = activeDevice?.id;
  const loadPlatform = activeDevice?.platform;
  const loadSessionId = iosSession?.sessionId;
  const load = useCallback(async (nextPath: string) => {
    if (!loadDeviceId || !loadPlatform || (loadPlatform === "ios" && !loadSessionId)) return;
    const deviceId = loadDeviceId;
    const sessionId = loadSessionId;
    const requestId = ++loadRequestRef.current;
    const normalizedPath = normalizeRemotePath(nextPath);
    if (loadPlatform === "ios") {
      const roots = iosAllowedRootsKey ? iosAllowedRootsKey.split("\u0001").map(normalizeRemotePath) : [];
      const matchingRoots = roots.filter((root) => isWithinRoot(normalizedPath, root));
      if (!matchingRoots.length) {
        const message = "请从已授权根目录进入，或重新连接并调整允许访问的根目录。";
        setFileError(message);
        notify("warning", "路径超出本次 SSH 会话范围", message);
        return;
      }
      setSelectedIosRoot((current) => {
        if (roots.includes(current) && isWithinRoot(normalizedPath, current)) return current;
        return [...matchingRoots].sort((left, right) => right.length - left.length)[0];
      });
    }
    setLoading(true);
    setFileError(undefined);
    setSelected(undefined);
    try {
      const result = loadPlatform === "android"
        ? await api.files(deviceId, normalizedPath)
        : await api.iosSshFiles(sessionId!, normalizedPath);
      if (!contextIsCurrent(deviceId, sessionId) || loadRequestRef.current !== requestId) return;
      filesPathByDevice.set(deviceId, normalizedPath);
      // The UI and subsequent file operations must retain the stable path the
      // user navigated through. Rootless jailbreaks can resolve `/var/mobile`
      // to a volatile `.jbroot-*` physical path, which is not necessarily a
      // valid SCP-visible path. A listing item's identity is its name below the
      // requested directory, so do not propagate a physical path returned by
      // an older backend/session into download, delete, or navigation calls.
      setEntries(loadPlatform === "ios"
        ? result.map((entry) => ({ ...entry, path: logicalRemoteChildPath(normalizedPath, entry.name) }))
        : result);
      setPath(normalizedPath);
      setPathInput(normalizedPath);
    } catch (error) {
      if (!contextIsCurrent(deviceId, sessionId) || loadRequestRef.current !== requestId) return;
      const message = error instanceof Error ? error.message : String(error);
      setFileError(message);
      notify("error", "无法读取目录", message);
    } finally {
      if (contextIsCurrent(deviceId, sessionId) && loadRequestRef.current === requestId) setLoading(false);
    }
  }, [contextIsCurrent, iosAllowedRootsKey, loadDeviceId, loadPlatform, loadSessionId, notify]);

  useEffect(() => {
    const fallback = loadPlatform === "ios" ? allowedIosRoots[0] ?? "/var/mobile" : "/sdcard";
    const cached = loadDeviceId ? filesPathByDevice.get(loadDeviceId) : undefined;
    const next = loadPlatform === "ios"
      ? cached && allowedIosRoots.some((root) => isWithinRoot(cached, root)) ? cached : fallback
      : cached ?? fallback;
    if (loadPlatform === "ios") {
      const matchingRoot = allowedIosRoots.filter((root) => isWithinRoot(next, root)).sort((left, right) => right.length - left.length)[0];
      setSelectedIosRoot(matchingRoot ?? normalizeRemotePath(fallback));
    }
    setPath(next);
    setPathInput(next);
    if (loadPlatform === "android" || (loadPlatform === "ios" && loadSessionId)) void load(next); else setEntries([]);
  }, [allowedIosRoots, load, loadDeviceId, loadPlatform, loadSessionId]);

  const filtered = useMemo(() => entries.filter((entry) => entry.name.toLowerCase().includes(search.toLowerCase())), [entries, search]);
  const selectedEntry = useMemo(() => entries.find((entry) => entry.path === selected), [entries, selected]);
  const crumbs = useMemo(() => {
    const normalizedPath = normalizeRemotePath(path);
    if (activeDevice?.platform !== "ios") return normalizedPath.split("/").filter(Boolean);
    if (!isWithinRoot(normalizedPath, currentIosRoot) || normalizedPath === currentIosRoot) return [];
    return normalizedPath.slice(currentIosRoot.length + 1).split("/").filter(Boolean);
  }, [activeDevice?.platform, currentIosRoot, path]);

  const navigateUp = () => {
    const boundary = activeDevice?.platform === "ios" ? currentIosRoot : "/";
    const normalizedPath = normalizeRemotePath(path);
    if (normalizedPath === boundary) return;
    const parent = normalizedPath.split("/").slice(0, -1).join("/") || "/";
    const next = activeDevice?.platform === "ios" && !isWithinRoot(parent, boundary) ? boundary : parent;
    void load(next);
  };

  const startTransfer = async () => {
    if (!activeDevice || !transfer) return;
    const device = activeDevice;
    const sessionId = iosSession?.sessionId;
    if (device.platform === "ios" && !sessionId) return;
    const operation = transfer;
    const operationLocalPath = localPath.trim();
    const operationRemotePath = remotePath.trim();
    const operationDirectory = path;
    setLoading(true);
    setFileError(undefined);
    try {
      const result = device.platform === "android"
        ? operation === "upload"
          ? await api.pushFile(device.id, operationLocalPath, operationRemotePath || operationDirectory, overwrite)
          : await api.pullFile(device.id, operationRemotePath, operationLocalPath, overwrite)
        : operation === "upload"
          ? await api.uploadIosSshFile(sessionId!, operationLocalPath, operationRemotePath || operationDirectory, overwrite)
          : await api.downloadIosSshFile(sessionId!, operationRemotePath, operationLocalPath, overwrite);
      if (!result.success) throw new Error(result.message);
      notify("success", operation === "upload" ? "上传完成" : "下载完成", `${device.name} · ${result.message}`);
      record(operation === "upload" ? "上传文件" : "下载文件", `${device.name} · ${operationRemotePath}`);
      if (!contextIsCurrent(device.id, sessionId)) return;
      setTransfer(null);
      await load(operationDirectory);
    } catch (error) {
      if (!contextIsCurrent(device.id, sessionId)) return;
      const message = error instanceof Error ? error.message : String(error);
      setFileError(message);
      notify("error", `${device.name} 传输失败`, message);
    } finally {
      if (contextIsCurrent(device.id, sessionId)) setLoading(false);
    }
  };

  const createFolder = async () => {
    if (!activeDevice || !folderName.trim()) return;
    const device = activeDevice;
    const sessionId = iosSession?.sessionId;
    if (device.platform === "ios" && !sessionId) return;
    const operationDirectory = path;
    const target = `${operationDirectory.replace(/\/$/, "")}/${folderName.trim()}`;
    setLoading(true);
    setFileError(undefined);
    try {
      const result = device.platform === "android" ? await api.mkdir(device.id, target) : await api.mkdirIosSsh(sessionId!, target);
      if (!result.success) throw new Error(result.message);
      notify("success", "目录已创建", target);
      if (!contextIsCurrent(device.id, sessionId)) return;
      setNewFolder(false); setFolderName(""); await load(operationDirectory);
    } catch (error) { if (!contextIsCurrent(device.id, sessionId)) return; const message = error instanceof Error ? error.message : String(error); setFileError(message); notify("error", `${device.name} 创建失败`, message); }
    finally { if (contextIsCurrent(device.id, sessionId)) setLoading(false); }
  };

  const confirmDelete = async () => {
    if (!activeDevice || !deleteTarget) return;
    const device = activeDevice;
    const sessionId = iosSession?.sessionId;
    if (device.platform === "ios" && !sessionId) return;
    const target = deleteTarget;
    const operationDirectory = path;
    setLoading(true);
    setFileError(undefined);
    try {
      const result = device.platform === "android"
        ? await api.deleteFile(device.id, target.path, target.kind === "directory")
        : await api.deleteIosSsh(sessionId!, target.path, target.kind === "directory");
      if (!result.success) throw new Error(result.message);
      notify("success", "设备端项目已删除", `${device.name} · ${target.name}`);
      record("删除设备文件", `${device.name} · ${target.path}`, "warning");
      if (!contextIsCurrent(device.id, sessionId)) return;
      setDeleteTarget(undefined); await load(operationDirectory);
    } catch (error) { if (!contextIsCurrent(device.id, sessionId)) return; const message = error instanceof Error ? error.message : String(error); setFileError(message); notify("error", `${device.name} 删除失败`, message); }
    finally { if (contextIsCurrent(device.id, sessionId)) setLoading(false); }
  };

  const currentIosPreset = (privateKeyPath = iosPrivateKey): IosSshPreset => ({
    mode: iosMode,
    authMode: iosAuthMode,
    host: iosHost.trim(),
    devicePort: iosDevicePort || "22",
    hostPort: iosHostPort,
    username: iosUsername.trim() || "root",
    privateKeyPath: privateKeyPath.trim(),
    allowedRoots: iosAllowedRoots.trim() || "/var/mobile",
  });

  const saveIosPreset = () => {
    if (!activeDevice || activeDevice.platform !== "ios") return;
    const preset = currentIosPreset();
    const roots = preset.allowedRoots.split(/[\n,]/).map((value) => value.trim()).filter(Boolean);
    if (!roots.length) { notify("warning", "至少填写一个允许访问的远端根目录"); return; }
    if (preset.mode === "lan" && !preset.host) { notify("warning", "请输入越狱设备的私网地址"); return; }
    writeIosSshPreset(activeDevice.id, preset);
    setIosPresetSaved(true);
    setIosSettingsOpen(false);
    notify("success", "iOS 连接设置已保存", "下次选择这台设备可直接一键连接；密码始终不会写入本地配置。");
  };

  const startIosSession = async (privateKeyOverride?: string, automatic = false) => {
    if (!activeDevice || activeDevice.platform !== "ios") return;
    const device = activeDevice;
    const preset = currentIosPreset(privateKeyOverride ?? iosPrivateKey);
    const roots = preset.allowedRoots.split(/[\n,]/).map((value) => value.trim()).filter(Boolean);
    if (preset.authMode === "password" && !iosPassword) { notify("warning", "请输入 SSH 密码"); return; }
    if (preset.authMode === "privateKey" && !preset.privateKeyPath) { notify("warning", "请选择 SSH 私钥"); return; }
    if (!roots.length) { notify("warning", "至少填写一个允许访问的远端根目录"); return; }
    if (preset.mode === "lan" && !preset.host) { notify("warning", "请输入越狱设备的私网地址"); return; }
    setIosConnecting(true);
    setFileError(undefined);
    try {
      const session = await api.startIosSshSession({
        transport: preset.mode === "usb"
          ? { mode: "usb", udid: device.id, devicePort: Number(preset.devicePort) || 22, ...(preset.hostPort ? { hostPort: Number(preset.hostPort) } : {}) }
          : { mode: "lan", host: preset.host, port: Number(preset.devicePort) || 22 },
        username: preset.username,
        authMode: preset.authMode,
        ...(preset.authMode === "password" ? { password: iosPassword } : { privateKeyPath: preset.privateKeyPath }),
        allowedRoots: roots,
      });
      if (!contextIsCurrent(device.id)) {
        await api.stopIosSshSession(session.sessionId).catch(() => undefined);
        return;
      }
      writeIosSshPreset(device.id, preset);
      setIosPresetSaved(true);
      onIosSessionChange(session);
      notify("success", "iOS SSH 会话已连接", `${session.authMode === "password" ? "密码" : "私钥"}认证 · ${session.tunnel ? `USB 隧道 127.0.0.1:${session.tunnel.hostPort}` : `${session.sshHost}:${session.sshPort}`}`);
      record("连接越狱 iOS SSH", `${device.name} · ${session.username}@${session.sshHost}:${session.sshPort}`);
    } catch (error) {
      if (!contextIsCurrent(device.id)) return;
      const message = error instanceof Error ? error.message : String(error);
      setFileError(message);
      notify("error", "iOS SSH 连接失败", message);
      if (automatic && contextIsCurrent(device.id)) setIosSettingsOpen(true);
    } finally {
      if (contextRef.current.startsWith(`${device.id}\u0000`)) setIosConnecting(false);
    }
  };

  const connectIosFileSystem = async () => {
    if (iosAuthMode === "password") {
      await startIosSession();
      return;
    }
    let privateKey = iosPrivateKey.trim();
    if (!privateKey) {
      const deviceId = activeDevice?.id;
      if (!deviceId) return;
      const selectedKey = await chooseLocalFile("选择用于自有越狱 iOS 设备的 SSH 私钥");
      if (!selectedKey || !contextIsCurrent(deviceId)) return;
      privateKey = selectedKey;
      setIosPrivateKey(selectedKey);
    }
    await startIosSession(privateKey);
  };

  useEffect(() => {
    if (
      activeDevice?.platform !== "ios"
      || (activeDevice.state !== "online" && activeDevice.state !== "registered")
      || iosSession
      || iosPresetReadyFor !== activeDevice.id
      || iosAuthMode !== "password"
    ) return;
    const attemptKey = `${activeDevice.id}\u0000${activeDevice.address ?? ""}`;
    if (autoAttemptedIosDevices.has(attemptKey)) return;
    autoAttemptedIosDevices.add(attemptKey);
    void startIosSession(undefined, true);
  }, [activeDevice?.address, activeDevice?.id, activeDevice?.platform, activeDevice?.state, iosAuthMode, iosPresetReadyFor, iosSession?.sessionId]);

  const stopIosSession = async () => {
    if (!iosSession || !activeDevice) return;
    const deviceId = activeDevice.id;
    const sessionId = iosSession.sessionId;
    try {
      await api.stopIosSshSession(sessionId);
      notify("success", "iOS SSH 会话已断开", "关联的 USB iproxy 隧道也已停止。");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setFileError(message);
      notify("error", "无法断开 SSH 会话", message);
      return;
    }
    onIosSessionChange(undefined);
    if (contextIsCurrent(deviceId, sessionId)) setEntries([]);
  };

  const selectIosPrivateKey = async () => {
    if (!activeDevice) return;
    const deviceId = activeDevice.id;
    const sessionId = iosSession?.sessionId;
    const selectedKey = await chooseLocalFile("选择用于自有越狱 iOS 设备的 SSH 私钥");
    if (selectedKey && contextIsCurrent(deviceId, sessionId)) setIosPrivateKey(selectedKey);
  };

  const selectTransferSource = async () => {
    if (!activeDevice) return;
    const deviceId = activeDevice.id;
    const sessionId = iosSession?.sessionId;
    const selectedPath = await chooseLocalFile("选择要上传到设备的文件");
    if (selectedPath && contextIsCurrent(deviceId, sessionId)) setLocalPath(selectedPath);
  };

  const selectTransferDestination = async () => {
    if (!activeDevice) return;
    const deviceId = activeDevice.id;
    const sessionId = iosSession?.sessionId;
    const selectedPath = await chooseDirectory("选择保存目录");
    if (selectedPath && contextIsCurrent(deviceId, sessionId)) setLocalPath(selectedPath);
  };

  if (!activeDevice) return (
    <div className="page files-page"><div className="page-heading"><div><span className="eyebrow">REMOTE FILE SYSTEM</span><h1>文件</h1><p>通过已授权连接管理设备可访问的文件。</p></div></div><Panel><EmptyState icon={<HardDrive size={30} />} title="请先选择设备" detail="文件操作始终绑定一台明确的目标设备。" /></Panel></div>
  );

  if (activeDevice.state !== "online" && !(activeDevice.platform === "ios" && activeDevice.state === "registered")) return (
    <div className="page files-page"><div className="page-heading"><div><span className="eyebrow">REMOTE FILE SYSTEM</span><h1>文件</h1><p>{activeDevice.name}</p></div></div><Panel><EmptyState icon={<HardDrive size={30} />} title="设备当前不可操作" detail="设备需要处于已连接并授权状态，才能读取或修改文件。" /></Panel></div>
  );

  if (activeDevice.platform === "ios" && !iosSession) return (
    <div className="page files-page">
      <div className="page-heading"><div><span className="eyebrow">JAILBROKEN IOS / SSH</span><h1>iOS 文件</h1><p>{activeDevice.name} · 一键建立受控 SSH 会话</p></div></div>
      <div className="ios-ssh-grid">
        <Panel className="span-7" title={<><KeyRound size={17} /> iOS 文件连接</>} action={iosPresetSaved ? <StatusBadge tone="success">已保存此设备</StatusBadge> : <StatusBadge>默认设置</StatusBadge>}>
          <div className="ios-connect-card">
            <div className="ios-connect-hero">
              <span className={`quick-icon ${iosMode === "usb" ? "quick-green" : "quick-blue"}`}>{iosMode === "usb" ? <Cable size={20} /> : <NetworkIcon size={20} />}</span>
              <div><strong>{iosMode === "usb" ? "USB + iproxy" : "局域网 SSH"}</strong><small>{iosMode === "usb" ? "自动创建仅本机可访问的 SSH 隧道" : "使用已登记的私网 SSH 地址"}</small></div>
            </div>
            <div className="ios-connect-summary" aria-label="iOS SSH 连接摘要">
              <div><span>登录目标</span><strong>{iosUsername.trim() || "root"}@{iosMode === "usb" ? "当前 USB 设备" : iosHost.trim() || "未设置地址"}</strong></div>
              <div><span>端口</span><strong>{iosMode === "usb" ? `设备 ${iosDevicePort || "22"} · 本机 ${iosHostPort || "自动"}` : iosDevicePort || "22"}</strong></div>
              <div><span>允许目录</span><strong>{iosAllowedRoots.split(/[\n,]/).map((value) => value.trim()).filter(Boolean).join(" · ") || "/var/mobile"}</strong></div>
              <div><span>身份凭据</span><strong>{iosAuthMode === "password" ? `${iosUsername.trim() || "root"} · 密码登录` : iosPrivateKey ? iosPrivateKey.split(/[\\/]/).pop() : "首次连接时选择私钥"}</strong></div>
            </div>
            <div className="ios-connect-actions">
              <Button className="ios-connect-primary" variant="primary" icon={iosConnecting ? <LoaderCircle className="spin" size={15} /> : <Link2 size={15} />} disabled={iosConnecting} onClick={() => void connectIosFileSystem()}>{iosConnecting ? "正在连接并读取目录…" : iosAuthMode === "password" ? "一键连接并打开文件" : iosPrivateKey.trim() ? "一键连接并打开文件" : "选择私钥并连接"}</Button>
              <Button variant="ghost" icon={<Settings2 size={15} />} onClick={() => setIosSettingsOpen((open) => !open)}>{iosSettingsOpen ? "收起连接设置" : "修改连接设置"}</Button>
            </div>
            {fileError && <InlineNotice tone="danger" title="连接未建立">{fileError}</InlineNotice>}
          </div>

          {iosSettingsOpen && <div className="form-stack ios-ssh-form ios-ssh-advanced">
            <InlineNotice tone="info" title="密码只驻留当前运行内存">默认使用 root / alpine；密码不会写入本地设置、日志或命令行。保存设置时只记住认证方式。</InlineNotice>
            <Tabs value={iosMode} onChange={setIosMode} options={[{ id: "usb", label: "USB + iproxy（推荐）" }, { id: "lan", label: "局域网 SSH" }]} />
            {iosMode === "lan" && <Field label="设备私网地址" hint="手工登记的 Wi-Fi 设备会自动带入地址；仅接受私网地址。"><input value={iosHost} onChange={(event) => setIosHost(event.target.value)} placeholder="192.168.1.42" /></Field>}
            <div className="field-row"><Field label="SSH 用户"><input value={iosUsername} onChange={(event) => setIosUsername(event.target.value)} placeholder="root" autoComplete="off" /></Field><Field label="设备 SSH 端口"><input value={iosDevicePort} onChange={(event) => setIosDevicePort(event.target.value.replace(/\D/g, "").slice(0, 5))} inputMode="numeric" /></Field></div>
            {iosMode === "usb" && <Field label="本机隧道端口（可留空）" hint="留空时自动选择空闲端口；iproxy 只绑定 127.0.0.1。"><input value={iosHostPort} onChange={(event) => setIosHostPort(event.target.value.replace(/\D/g, "").slice(0, 5))} placeholder="自动" inputMode="numeric" /></Field>}
            <div className="field"><span className="field-label">认证方式</span><Tabs value={iosAuthMode} onChange={setIosAuthMode} options={[{ id: "password", label: "账号密码（默认）" }, { id: "privateKey", label: "SSH 私钥" }]} /></div>
            {iosAuthMode === "password"
              ? <Field label="SSH 密码" hint="默认 alpine；仅驻留当前运行内存，切换设备时恢复默认值。"><input type="password" value={iosPassword} onChange={(event) => setIosPassword(event.target.value)} autoComplete="off" /></Field>
              : <Field label="SSH 私钥" hint="使用 OpenSSH 私钥与无交互 BatchMode。"><div className="path-input"><input value={iosPrivateKey} onChange={(event) => setIosPrivateKey(event.target.value)} placeholder="首次连接时可直接从主按钮选择" /><button type="button" onClick={() => void selectIosPrivateKey()}>选择</button></div></Field>}
            <Field label="允许访问的根目录" hint="多个目录用逗号分隔；不能删除根目录本身，也不能经符号链接越界。"><input value={iosAllowedRoots} onChange={(event) => setIosAllowedRoots(event.target.value)} placeholder="/var/mobile" /></Field>
            <div className="ios-advanced-actions"><Button icon={<Save size={15} />} onClick={saveIosPreset}>保存此设备设置</Button><Button variant="primary" icon={iosConnecting ? <LoaderCircle className="spin" size={15} /> : <Link2 size={15} />} disabled={iosConnecting} onClick={() => void connectIosFileSystem()}>{iosConnecting ? "正在连接…" : "连接并打开文件"}</Button></div>
          </div>
          }
        </Panel>
        <Panel className="span-5" title="连接方式与边界">
          <div className="ios-ssh-paths"><div><span className="quick-icon quick-green"><Cable size={19} /></span><span><strong>USB + iproxy</strong><small>UDID 固定到当前设备，自动管理回环隧道；无需把 SSH 暴露到 Wi-Fi。</small></span></div><div><span className="quick-icon quick-blue"><NetworkIcon size={19} /></span><span><strong>局域网 SSH</strong><small>只允许明确填写的私网目标；适合没有 USB 链路的实验设备。</small></span></div><div><span className="quick-icon quick-purple"><ShieldCheck size={19} /></span><span><strong>路径白名单</strong><small>列表、传输、新建和删除都限制到本次会话声明的根目录。</small></span></div></div>
          <div className="security-footnote"><ShieldAlert size={15} /><span>首次连接会使用 accept-new 记录主机密钥；密钥变化时 SSH 会拒绝连接。</span></div>
        </Panel>
      </div>
    </div>
  );

  return (
    <div className="page files-page">
      <div className="page-heading">
        <div><span className="eyebrow">REMOTE FILE SYSTEM</span><h1>文件</h1><p>{activeDevice.name} · {activeDevice.platform === "android" ? "ADB Shell 可访问范围" : `SSH ${iosSession?.username}@${iosSession?.sshHost}:${iosSession?.sshPort} · ${iosSession?.authMode === "password" ? "密码" : "私钥"}认证`}</p></div>
        <div className="heading-actions"><Button icon={<ArrowDownToLine size={15} />} disabled={!selectedEntry || selectedEntry.kind !== "file"} onClick={() => { if (!selectedEntry || selectedEntry.kind !== "file") return; setOverwrite(false); setTransfer("download"); setRemotePath(selectedEntry.path); }}>下载</Button><Button icon={<ArrowUpFromLine size={15} />} onClick={() => { setOverwrite(false); setTransfer("upload"); setRemotePath(path); }}>上传</Button><Button variant="primary" icon={<Plus size={15} />} onClick={() => setNewFolder(true)}>新建目录</Button>{activeDevice.platform === "ios" && <Button variant="ghost" icon={<CircleStop size={14} />} onClick={() => void stopIosSession()}>断开 SSH</Button>}</div>
      </div>

      <Panel className="file-manager-panel">
        <div className="file-toolbar">
          <div className="nav-buttons"><button className="icon-button" onClick={navigateUp} disabled={normalizeRemotePath(path) === (activeDevice.platform === "ios" ? currentIosRoot : "/")} title="上一级"><ArrowLeft size={16} /></button><button className="icon-button" onClick={() => load(path)} title="刷新"><RefreshCw className={loading ? "spin" : ""} size={16} /></button></div>
          <form className="path-bar" onSubmit={(event) => { event.preventDefault(); void load(pathInput); }}><Home size={15} /><input value={pathInput} onChange={(e) => setPathInput(e.target.value)} aria-label="设备路径" /><button type="submit"><ArrowRight size={15} /></button></form>
          <div className="search-input"><Search size={15} /><input value={search} onChange={(e) => setSearch(e.target.value)} placeholder="筛选当前目录" /></div>
        </div>
        <div className="breadcrumb">
          {activeDevice.platform === "ios" ? <>
            <button onClick={() => void load(currentIosRoot)}>允许根目录</button>
            {allowedIosRoots.map((root) => <span key={root}><ChevronRight size={13} /><button aria-current={root === currentIosRoot ? "location" : undefined} title={`切换到 ${root}`} onClick={() => { setSelectedIosRoot(root); void load(root); }}>{root === currentIosRoot ? `● ${root}` : root}</button></span>)}
          </> : <button onClick={() => void load("/")}>设备根目录</button>}
          {crumbs.map((crumb, index) => <span key={`${crumb}-${index}`}><ChevronRight size={13} /><button onClick={() => void load(activeDevice.platform === "ios" ? `${currentIosRoot}/${crumbs.slice(0, index + 1).join("/")}` : `/${crumbs.slice(0, index + 1).join("/")}`)}>{crumb}</button></span>)}
        </div>
        {fileError && <div className="file-inline-error"><InlineNotice tone="danger" title="文件操作未完成">{fileError}</InlineNotice></div>}
        <div className="file-split-label"><span><HardDrive size={15} /> {activeDevice.name}</span><span>{filtered.length} 个项目</span></div>
        <div className="data-table file-table">
          <div className="data-row data-head"><span>名称</span><span>大小</span><span>修改时间</span><span>权限 / 所有者</span><span /></div>
          {loading && !entries.length ? <div className="file-loading"><LoaderCircle className="spin" size={20} />正在读取目录…</div> : filtered.map((entry) => (
            <div className={`data-row ${selected === entry.path ? "selected" : ""}`} role="button" tabIndex={0} key={entry.path} onClick={() => setSelected(entry.path)} onDoubleClick={() => (entry.kind === "directory" || entry.kind === "link") && load(entry.path)} onKeyDown={(event) => { if (event.key === "Enter") entry.kind === "directory" || entry.kind === "link" ? void load(entry.path) : setSelected(entry.path); }}>
              <span className="file-name" title={entry.linkTarget ? `链接到 ${entry.linkTarget}` : entry.name}>{fileIcon(entry)}<strong>{entry.name}</strong>{entry.kind === "directory" && <StatusBadge>目录</StatusBadge>}{entry.kind === "link" && <StatusBadge tone="info">链接</StatusBadge>}</span>
              <span>{entry.kind === "directory" ? "—" : formatBytes(entry.size)}</span>
              <span>{entry.modified ?? "—"}</span>
              <span><code>{entry.permissions ?? "—"}</code><small>{entry.owner ?? ""}</small></span>
              <span className="row-actions"><button className="icon-button danger-icon" onClick={(e) => { e.stopPropagation(); setDeleteTarget(entry); }} aria-label={`删除 ${entry.name}`}><Trash2 size={15} /></button></span>
            </div>
          ))}
          {!loading && !filtered.length && <EmptyState icon={<FolderOpen size={27} />} title={search ? "没有匹配项目" : "目录为空"} detail={search ? "清除筛选词后查看全部项目。" : "可以上传文件或在此创建新目录。"} />}
        </div>
        <footer className="file-status"><span><FolderLock size={14} /> 当前权限：{activeDevice.platform === "ios" ? `SSH UID ${iosSession?.remoteUid ?? "未知"} · 路径白名单` : activeDevice.rooted ? "Root（写入仍需确认）" : "ADB Shell"}</span><span>{activeDevice.platform === "ios" && iosSession?.tunnel ? `iproxy 127.0.0.1:${iosSession.tunnel.hostPort} → ${iosSession.tunnel.devicePort}` : "路径不会自动递归扫描"}</span></footer>
      </Panel>

      {transfer && <Modal title={transfer === "upload" ? "上传到设备" : "从设备下载"} subtitle={`目标设备：${activeDevice.name}`} onClose={() => setTransfer(null)} footer={<><Button onClick={() => setTransfer(null)}>取消</Button><Button variant="primary" disabled={loading || !localPath.trim() || !remotePath.trim()} onClick={startTransfer}>{loading ? "传输中…" : transfer === "upload" ? "开始上传" : "开始下载"}</Button></>}>
        <div className="form-stack">
          {transfer === "upload" ? <><Field label="本机文件" hint="路径会按参数传给工具，不会经过本机 Shell。"><div className="path-input"><input value={localPath} onChange={(e) => setLocalPath(e.target.value)} placeholder="选择要上传的文件" /><button type="button" onClick={() => void selectTransferSource()}>选择</button></div></Field><Field label="设备目标路径"><input value={remotePath} onChange={(e) => setRemotePath(e.target.value)} /></Field></> : <><Field label="设备文件路径"><input value={remotePath} onChange={(e) => setRemotePath(e.target.value)} /></Field><Field label="保存到本机"><div className="path-input"><input value={localPath} onChange={(e) => setLocalPath(e.target.value)} placeholder="选择保存目录" /><button type="button" onClick={() => void selectTransferDestination()}>选择</button></div></Field></>}
          <label className="check-row"><input type="checkbox" checked={overwrite} onChange={(event) => setOverwrite(event.target.checked)} /><span>如果存在同名文件，允许覆盖</span></label>
        </div>
      </Modal>}

      {newFolder && <Modal title="新建设备目录" subtitle={path} onClose={() => setNewFolder(false)} footer={<><Button onClick={() => setNewFolder(false)}>取消</Button><Button variant="primary" disabled={!folderName.trim() || loading} onClick={createFolder}>创建</Button></>}><Field label="目录名称"><input autoFocus value={folderName} onChange={(e) => setFolderName(e.target.value)} placeholder="new-folder" /></Field></Modal>}

      {deleteTarget && <Modal title="永久删除设备端项目？" subtitle={`${activeDevice.name} · ${deleteTarget.path}`} onClose={() => setDeleteTarget(undefined)} footer={<><Button onClick={() => setDeleteTarget(undefined)}>取消</Button><Button variant="danger" disabled={loading} onClick={confirmDelete}>确认永久删除</Button></>}>
        <div className="danger-confirm"><ShieldAlert size={25} /><div><strong>这项操作无法通过本机回收站恢复</strong><p>{deleteTarget.kind === "directory" ? "目录及其内容将从设备端递归删除。" : "文件将从设备端永久删除。"}</p></div></div>
      </Modal>}
    </div>
  );
}
