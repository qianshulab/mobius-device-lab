import { Cable, Camera, ChevronRight, Clipboard, KeyRound, LoaderCircle, MonitorSmartphone, Pause, Play, RefreshCw, Search, ShieldCheck, Smartphone, Trash2, Usb, Video, Wifi } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { Button, DeviceIdentity, EmptyState, Modal, Panel, StatusBadge, StatusDot, Tabs } from "../components/Ui";
import { api } from "../lib/api";
import { chooseDirectory } from "../lib/dialog";
import type { ActivityItem, AndroidScreenRecordingSession, AndroidScreenStream, Device, IosScreenCapability, MediaCaptureResult, ScreenFrame, ToastMessage, ToolHealth } from "../types";

type ActiveRecording = AndroidScreenRecordingSession & { deviceName: string };

interface RecordingTarget {
  serial: string;
  deviceName: string;
  rooted: boolean;
}

type RecordingRuntimeState =
  | { phase: "idle" }
  | { phase: "starting"; target: RecordingTarget }
  | { phase: "recording"; session: ActiveRecording }
  | { phase: "stopping"; session: ActiveRecording };

let recordingRuntimeState: RecordingRuntimeState = { phase: "idle" };
let recordingStartRequest: Promise<ActiveRecording | undefined> | undefined;
const recordingStateSubscribers = new Set<(state: RecordingRuntimeState) => void>();
const recordingStopRequests = new Map<string, Promise<MediaCaptureResult>>();

function publishRecordingState(state: RecordingRuntimeState) {
  recordingRuntimeState = state;
  recordingStateSubscribers.forEach((subscriber) => subscriber(state));
}

function clearMatchingRecordingStart(target: RecordingTarget) {
  const current = recordingRuntimeState;
  if (current.phase === "starting" && current.target.serial === target.serial) publishRecordingState({ phase: "idle" });
}

function requestRecordingStart(target: RecordingTarget, resolveDirectory: () => Promise<string | null | undefined>) {
  if (recordingStartRequest) return recordingStartRequest;
  if (recordingRuntimeState.phase !== "idle") {
    return Promise.resolve(recordingRuntimeState.phase === "recording" || recordingRuntimeState.phase === "stopping" ? recordingRuntimeState.session : undefined);
  }

  publishRecordingState({ phase: "starting", target });
  const request = (async () => {
    try {
      const directory = await resolveDirectory();
      if (!directory) {
        clearMatchingRecordingStart(target);
        return undefined;
      }
      const started = await api.startAndroidScreenRecording(target.serial, directory, 8_000_000, target.rooted);
      if (!started.success) throw new Error(started.message);
      const session: ActiveRecording = { ...started, deviceName: target.deviceName };
      publishRecordingState({ phase: "recording", session });
      return session;
    } catch (error) {
      clearMatchingRecordingStart(target);
      throw error;
    }
  })();
  recordingStartRequest = request;
  const clearRequest = () => { if (recordingStartRequest === request) recordingStartRequest = undefined; };
  void request.then(clearRequest, clearRequest);
  return request;
}

function requestRecordingStop(session: ActiveRecording) {
  const existing = recordingStopRequests.get(session.sessionId);
  if (existing) return existing;
  if (recordingRuntimeState.phase === "recording" && recordingRuntimeState.session.sessionId === session.sessionId) {
    publishRecordingState({ phase: "stopping", session });
  }
  const request = api.stopAndroidScreenRecording(session.serial, session.sessionId)
    .then((result) => {
      if (!result.success) throw new Error(result.message);
      if ((recordingRuntimeState.phase === "recording" || recordingRuntimeState.phase === "stopping") && recordingRuntimeState.session.sessionId === session.sessionId) {
        publishRecordingState({ phase: "idle" });
      }
      return result;
    })
    .catch((error) => {
      if ((recordingRuntimeState.phase === "recording" || recordingRuntimeState.phase === "stopping") && recordingRuntimeState.session.sessionId === session.sessionId) {
        publishRecordingState({ phase: "recording", session });
      }
      throw error;
    })
    .finally(() => recordingStopRequests.delete(session.sessionId));
  recordingStopRequests.set(session.sessionId, request);
  return request;
}

function recordingTime(seconds: number) {
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const remainder = seconds % 60;
  return hours > 0
    ? [hours, minutes, remainder].map((value) => String(value).padStart(2, "0")).join(":")
    : [minutes, remainder].map((value) => String(value).padStart(2, "0")).join(":");
}

interface DevicesProps {
  devices: Device[];
  activeDevice?: Device;
  loading: boolean;
  onRefresh: () => void;
  onSelect: (id: string) => void;
  onAction: (action: "scan" | "pair" | "connect" | "ios" | "scrcpy") => void;
  onForgetRegisteredIos: (id: string) => void;
  tools: ToolHealth[];
  mediaDirectory: string;
  onMediaDirectoryChange: (directory: string) => void;
  notify: (type: ToastMessage["type"], title: string, detail?: string) => void;
  record: (title: string, detail: string, status?: ActivityItem["status"]) => void;
}

export default function Devices({ devices, activeDevice, loading, onRefresh, onSelect, onAction, onForgetRegisteredIos, tools, mediaDirectory, onMediaDirectoryChange, notify, record }: DevicesProps) {
  const [filter, setFilter] = useState<"all" | "android" | "ios">("all");
  const [search, setSearch] = useState("");
  const [deviceManagerOpen, setDeviceManagerOpen] = useState(false);
  const [mediaBusy, setMediaBusy] = useState<"clipboard" | "screenshot">();
  const [recordingState, setRecordingState] = useState<RecordingRuntimeState>(() => recordingRuntimeState);
  const [recordingElapsed, setRecordingElapsed] = useState(0);
  const [screenFrame, setScreenFrame] = useState<ScreenFrame>();
  const [screenFrameBusy, setScreenFrameBusy] = useState(false);
  const [screenPreviewPaused, setScreenPreviewPaused] = useState(false);
  const [screenPreviewError, setScreenPreviewError] = useState<string>();
  const [screenRefreshKey, setScreenRefreshKey] = useState(0);
  const [androidStream, setAndroidStream] = useState<AndroidScreenStream>();
  const [androidStreamBusy, setAndroidStreamBusy] = useState(false);
  const [androidStreamError, setAndroidStreamError] = useState<string>();
  const [liveDimensions, setLiveDimensions] = useState<{ width: number; height: number }>();
  const [iosScreenCapability, setIosScreenCapability] = useState<IosScreenCapability>();
  const [iosCapabilityBusy, setIosCapabilityBusy] = useState(false);
  const screenRequestRef = useRef(0);
  const streamRequestRef = useRef(0);
  const liveStreamImageRef = useRef<HTMLImageElement>(null);
  const iosCapabilityRequestRef = useRef(0);
  const filtered = useMemo(() => devices.filter((device) => {
    const matchesPlatform = filter === "all" || device.platform === filter;
    const query = search.toLowerCase();
    const matchesSearch = !query || `${device.name} ${device.id} ${device.address ?? ""} ${device.model ?? ""}`.toLowerCase().includes(query);
    return matchesPlatform && matchesSearch;
  }), [devices, filter, search]);
  const androidReady = activeDevice?.platform === "android" && activeDevice.state === "online";
  const scrcpyReady = tools.some((tool) => tool.id === "scrcpy" && tool.state === "ready");
  const ffmpegReady = tools.some((tool) => tool.id === "ffmpeg" && tool.state === "ready");
  const iosScreenToolReady = tools.some((tool) => ["ios", "idevicescreenshot"].includes(tool.id) && tool.state === "ready");
  const iosNativeCandidate = activeDevice?.platform === "ios"
    && activeDevice.state === "online"
    && activeDevice.connectionSource !== "manual"
    && !activeDevice.id.startsWith("ios-ssh:");
  const iosScreenReady = !!iosNativeCandidate && !!iosScreenCapability?.available && iosScreenToolReady;
  const screenReady = androidReady || iosScreenReady;
  const androidScreenOffline = activeDevice?.platform === "android" && !androidReady;
  const iosScreenGuidance = activeDevice?.platform !== "ios"
    ? ""
    : activeDevice.connectionSource === "manual" || activeDevice.id.startsWith("ios-ssh:")
      ? "当前是仅 SSH 端点；iOS 截图服务不经 SSH 提供。请改用 USB/usbmux 或已配对的网络连接。"
      : !iosScreenToolReady
        ? "iOS 截图工具未就绪；请重新检测随包 go-ios，或配置 libimobiledevice 工具目录。"
        : iosCapabilityBusy
          ? "正在检测已配对的 iOS 屏幕服务…"
          : iosScreenCapability?.available
            ? `${iosScreenCapability.transport === "network" ? "已配对网络" : "USB/usbmux"} · screenshotr 采样预览`
            : "未在已配对的 USB 或网络连接中找到此 UDID；连接、解锁并信任设备后重试。";
  const browserStreamPreview = androidStream?.transport === "browser-preview";
  const previewDimensions = androidStream
    ? browserStreamPreview
      ? androidStream.width && androidStream.height ? { width: androidStream.width, height: androidStream.height } : undefined
      : liveDimensions ?? (androidStream.width && androidStream.height ? { width: androidStream.width, height: androidStream.height } : undefined)
    : screenFrame ? { width: screenFrame.width, height: screenFrame.height } : undefined;
  const contentIsLandscape = !!previewDimensions && previewDimensions.width > previewDimensions.height;
  const previewAspectRatio = "9 / 20";
  const recordingSession = recordingState.phase === "recording" || recordingState.phase === "stopping" ? recordingState.session : undefined;
  const recordingBusy = recordingState.phase === "starting" ? "starting" : recordingState.phase === "stopping" ? "stopping" : undefined;
  const recordingDeviceName = recordingState.phase === "starting" ? recordingState.target.deviceName : recordingSession?.deviceName;
  const recordingRuntimeActive = recordingState.phase !== "idle";

  useEffect(() => {
    const updateState = (state: RecordingRuntimeState) => setRecordingState(state);
    recordingStateSubscribers.add(updateState);
    updateState(recordingRuntimeState);
    return () => {
      recordingStateSubscribers.delete(updateState);
    };
  }, []);

  useEffect(() => {
    if (!recordingSession) {
      setRecordingElapsed(0);
      return;
    }
    const updateElapsed = () => setRecordingElapsed(Math.max(0, Math.floor((Date.now() - recordingSession.startedAtMs) / 1000)));
    updateElapsed();
    const timer = window.setInterval(updateElapsed, 1000);
    return () => window.clearInterval(timer);
  }, [recordingSession]);

  useEffect(() => {
    screenRequestRef.current += 1;
    setScreenFrame(undefined);
    setScreenPreviewError(undefined);
    setScreenPreviewPaused(false);
    setScreenFrameBusy(false);
    setAndroidStream(undefined);
    setAndroidStreamBusy(false);
    setAndroidStreamError(undefined);
    setLiveDimensions(undefined);
    setIosScreenCapability(undefined);
    setIosCapabilityBusy(false);
  }, [activeDevice?.id, activeDevice?.state]);

  useEffect(() => {
    if (!androidStream || androidStream.transport === "browser-preview") {
      setLiveDimensions(undefined);
      return;
    }
    const update = () => {
      const image = liveStreamImageRef.current;
      if (!image?.naturalWidth || !image.naturalHeight) return;
      setLiveDimensions((current) => current?.width === image.naturalWidth && current.height === image.naturalHeight
        ? current
        : { width: image.naturalWidth, height: image.naturalHeight });
    };
    update();
    const timer = window.setInterval(update, 500);
    return () => window.clearInterval(timer);
  }, [androidStream?.sessionId, androidStream?.transport]);

  useEffect(() => {
    const target = activeDevice;
    const request = ++streamRequestRef.current;
    let disposed = false;
    let started: AndroidScreenStream | undefined;
    if (!target || target.platform !== "android" || target.state !== "online" || screenPreviewPaused || !scrcpyReady || !ffmpegReady) {
      setAndroidStream(undefined);
      setAndroidStreamBusy(false);
      return;
    }
    setAndroidStream(undefined);
    setAndroidStreamBusy(true);
    setAndroidStreamError(undefined);
    void api.startAndroidScreenStream(target.id, 720, 4_000_000, 20)
      .then((stream) => {
        started = stream;
        if (disposed || request !== streamRequestRef.current) {
          return api.stopAndroidScreenStream(target.id, stream.sessionId).catch(() => undefined);
        }
        setAndroidStream(stream);
        setScreenFrame(undefined);
        setScreenPreviewError(undefined);
        return undefined;
      })
      .catch((error) => {
        if (disposed || request !== streamRequestRef.current) return;
        setAndroidStreamError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => {
        if (!disposed && request === streamRequestRef.current) setAndroidStreamBusy(false);
      });
    return () => {
      disposed = true;
      streamRequestRef.current += 1;
      if (started) void api.stopAndroidScreenStream(target.id, started.sessionId).catch(() => undefined);
    };
  }, [activeDevice?.id, activeDevice?.state, screenPreviewPaused, screenRefreshKey, scrcpyReady, ffmpegReady]);

  useEffect(() => {
    const request = ++iosCapabilityRequestRef.current;
    if (!iosNativeCandidate || !activeDevice || !iosScreenToolReady) {
      setIosScreenCapability(undefined);
      setIosCapabilityBusy(false);
      return;
    }
    setIosCapabilityBusy(true);
    void api.probeIosScreenCapability(activeDevice.id)
      .then((capability) => {
        if (request === iosCapabilityRequestRef.current) setIosScreenCapability(capability);
      })
      .catch((error) => {
        if (request !== iosCapabilityRequestRef.current) return;
        setIosScreenCapability({
          available: false,
          transport: "unavailable",
          message: error instanceof Error ? error.message : String(error),
        });
      })
      .finally(() => {
        if (request === iosCapabilityRequestRef.current) setIosCapabilityBusy(false);
      });
    return () => { iosCapabilityRequestRef.current += 1; };
  }, [activeDevice?.id, iosNativeCandidate, iosScreenToolReady]);

  useEffect(() => {
    const useSamplePreview = activeDevice?.platform === "ios"
      ? screenReady
      : androidReady && (!scrcpyReady || !ffmpegReady || !!androidStreamError);
    if (!useSamplePreview || !activeDevice || screenPreviewPaused) {
      setScreenFrameBusy(false);
      return;
    }
    const target = activeDevice;
    const generation = ++screenRequestRef.current;
    let disposed = false;
    let timer: number | undefined;
    const schedule = (delay: number) => {
      if (!disposed) timer = window.setTimeout(() => void capture(), delay);
    };
    const capture = async () => {
      if (disposed || generation !== screenRequestRef.current) return;
      if (document.hidden) {
        schedule(1200);
        return;
      }
      setScreenFrameBusy(true);
      try {
        const frame = target.platform === "android"
          ? await api.captureAndroidScreenFrame(target.id)
          : await api.captureIosScreenFrame(target.id);
        if (disposed || generation !== screenRequestRef.current) return;
        setScreenFrame(frame);
        setScreenPreviewError(undefined);
        schedule(target.platform === "android" ? 1800 : 1800);
      } catch (error) {
        if (disposed || generation !== screenRequestRef.current) return;
        setScreenPreviewError(error instanceof Error ? error.message : String(error));
        schedule(target.platform === "android" ? 3000 : 5000);
      } finally {
        if (!disposed && generation === screenRequestRef.current) setScreenFrameBusy(false);
      }
    };
    void capture();
    return () => {
      disposed = true;
      screenRequestRef.current += 1;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [activeDevice?.id, screenReady, androidReady, scrcpyReady, ffmpegReady, androidStreamError, screenPreviewPaused, screenRefreshKey]);

  const resolveMediaDirectory = async () => {
    if (mediaDirectory) return mediaDirectory;
    const selected = await chooseDirectory("选择截图与录屏保存目录");
    if (selected) onMediaDirectoryChange(selected);
    return selected;
  };

  const reportMediaWarnings = (warnings: string[] | undefined, title: string) => {
    if (!warnings?.length) return;
    notify("warning", `${title}，但有提示`, warnings.join("；"));
    record(`${title}提示`, warnings.join("；"), "warning");
  };

  const handleAndroidStreamError = () => {
    const stream = androidStream;
    if (!stream || activeDevice?.platform !== "android") return;
    setAndroidStream(undefined);
    setAndroidStreamError("实时视频链路已断开，已切换为低频画面采样；可点击“重连”再试。");
    void api.stopAndroidScreenStream(activeDevice.id, stream.sessionId).catch(() => undefined);
  };

  const handleAndroidStreamLoad = () => {
    const image = liveStreamImageRef.current;
    if (!image?.naturalWidth || !image.naturalHeight) return;
    setLiveDimensions((current) => current?.width === image.naturalWidth && current.height === image.naturalHeight
      ? current
      : { width: image.naturalWidth, height: image.naturalHeight });
  };

  const captureScreenshot = async (copyToClipboard: boolean) => {
    if (!activeDevice || !screenReady || mediaBusy) return;
    const target = activeDevice;
    const mode = copyToClipboard ? "clipboard" : "screenshot";
    setMediaBusy(mode);
    try {
      const directory = copyToClipboard ? undefined : await resolveMediaDirectory();
      if (!copyToClipboard && !directory) return;
      const result = target.platform === "android"
        ? await api.captureAndroidScreenshot(target.id, directory ?? undefined, copyToClipboard)
        : await api.captureIosScreenshot(target.id, directory ?? undefined, copyToClipboard);
      if (!result.success) throw new Error(result.message);
      const detail = result.copiedToClipboard
        ? `已复制 · ${result.width ?? "?"}×${result.height ?? "?"}`
        : result.savedPath ?? result.message;
      const title = copyToClipboard ? "截图已复制到剪贴板" : "截图已保存到电脑";
      notify("success", title, detail);
      record(copyToClipboard ? "截图到剪贴板" : "截图保存到电脑", `${target.name} · ${result.savedPath ?? "系统剪贴板"}`);
      reportMediaWarnings(result.warnings, title);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "设备截图失败", message);
      record("设备截图失败", `${target.name} · ${message}`, "error");
    } finally {
      setMediaBusy(undefined);
    }
  };

  const startRecording = async () => {
    if (!activeDevice || !androidReady || mediaBusy || recordingRuntimeState.phase !== "idle" || recordingStartRequest) return;
    const target = activeDevice;
    try {
      const session = await requestRecordingStart({ serial: target.id, deviceName: target.name, rooted: !!target.rooted }, resolveMediaDirectory);
      if (!session) return;
      notify("info", "录屏已开始", "完成后点击同一按钮停止并保存");
      record("开始设备录屏", `${target.name} · 用户停止时保存`, "running");
      reportMediaWarnings(session.warnings, "录屏已开始");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "启动设备录屏失败", message);
      record("启动设备录屏失败", `${target.name} · ${message}`, "error");
    }
  };

  const stopRecording = async () => {
    const current = recordingRuntimeState;
    if (current.phase !== "recording") return;
    const session = current.session;
    try {
      const result = await requestRecordingStop(session);
      notify("success", "录屏已停止并保存到电脑", result.savedPath ?? result.message);
      record("停止并保存设备录屏", `${session.deviceName} · ${result.savedPath ?? session.plannedSavedPath}`);
      reportMediaWarnings(result.warnings, "录屏已保存");
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      notify("error", "停止并保存录屏失败", `${message}；会话已保留，可重试。`);
      record("停止并保存设备录屏失败", `${session.deviceName} · ${message}`, "error");
    }
  };

  const screenStatus = !activeDevice
    ? { dot: "muted" as const, label: "等待设备" }
    : androidScreenOffline
      ? { dot: "muted" as const, label: "设备离线" }
      : screenPreviewError || androidStreamError
        ? { dot: "warning" as const, label: activeDevice.platform === "android" ? "采样画面" : "等待重试" }
        : screenPreviewPaused
          ? { dot: "muted" as const, label: "已暂停" }
          : androidStream || screenFrame
            ? { dot: "success" as const, label: activeDevice.platform === "android" ? browserStreamPreview ? "布局预览" : androidStream ? "scrcpy 实时" : "画面采样" : "iOS 屏幕" }
            : { dot: "running" as const, label: "正在连接" };

  const screenWorkbench = (
    <Panel title={<><MonitorSmartphone size={17} /> 实时屏幕</>} action={<span className="panel-summary">固定纵向视图</span>} className="screen-panel">
      <div className="screen-workbench">
        <div className="screen-preview-shell">
          <div className="screen-preview-toolbar" title={androidStreamError || screenPreviewError || (activeDevice?.platform === "ios" ? iosScreenGuidance : contentIsLandscape ? "横屏内容已完整缩放到固定纵向手机窗口；需要更大画面时可使用键鼠控制窗口。" : undefined)}><span><StatusDot status={screenStatus.dot} />{screenStatus.label}</span><span>{contentIsLandscape ? "横屏内容 · 已适配" : previewDimensions ? `${previewDimensions.width}×${previewDimensions.height}` : "9:20"}</span></div>
          <div className={`screen-preview-canvas portrait ${!androidStream && screenFrame?.sizeBytes === 0 ? "preview-placeholder" : ""}`} style={{ aspectRatio: previewAspectRatio }}>
            {!activeDevice ? <div className="screen-preview-empty"><MonitorSmartphone size={30} /><strong>连接设备后自动投屏</strong><small>从右侧选择连接方式，完成后自动返回工作台。</small></div> : browserStreamPreview ? <div className="screen-device-frame portrait"><div className="screen-browser-preview"><img src="/brand/mobius-mark.png" alt="" /><strong>纵向布局预览</strong><small>原生应用会在这里显示 scrcpy 实时画面</small></div></div> : androidStream ? <div className="screen-device-frame portrait"><img ref={liveStreamImageRef} className="screen-live-stream" src={androidStream.streamUrl} alt={`${activeDevice.name} 实时屏幕`} draggable={false} onLoad={handleAndroidStreamLoad} onError={handleAndroidStreamError} /></div> : screenFrame ? <div className="screen-device-frame portrait"><img src={screenFrame.imageDataUrl} alt={`${activeDevice.name} 当前屏幕`} draggable={false} /></div> : <div className="screen-preview-empty">{screenFrameBusy || iosCapabilityBusy || androidStreamBusy ? <LoaderCircle className="spin" size={26} /> : <MonitorSmartphone size={28} />}<strong>{screenPreviewError || androidStreamError ? activeDevice.platform === "android" ? "实时链路不可用，正在获取采样画面" : "暂时无法取得画面" : screenReady ? "正在连接屏幕…" : activeDevice.platform === "ios" ? "需要已配对的 iOS 屏幕链路" : "设备当前不在线"}</strong>{(screenPreviewError || androidStreamError) ? <small title={screenPreviewError || androidStreamError}>{screenPreviewError || androidStreamError}</small> : activeDevice.platform === "ios" && <small>{iosScreenGuidance}</small>}</div>}
          </div>
          <div className="screen-preview-controls">{activeDevice ? <><Button variant="ghost" icon={screenPreviewPaused ? <Play size={14} /> : <Pause size={14} />} disabled={!screenReady} onClick={() => setScreenPreviewPaused((paused) => !paused)}>{screenPreviewPaused ? "继续" : "暂停"}</Button><Button variant="ghost" icon={<RefreshCw className={screenFrameBusy || iosCapabilityBusy || androidStreamBusy ? "spin" : ""} size={14} />} disabled={!screenReady || screenFrameBusy || androidStreamBusy} onClick={() => { setScreenPreviewPaused(false); setScreenPreviewError(undefined); setAndroidStreamError(undefined); setScreenRefreshKey((value) => value + 1); }}>{activeDevice.platform === "android" ? "重连" : "刷新"}</Button></> : <span>选择设备后自动显示</span>}</div>
        </div>
        <div className="screen-control-pane">
          <div className="screen-control-heading"><div><span>QUICK ACTIONS</span><strong>屏幕与媒体</strong></div><small>点击即可执行</small></div>
          {activeDevice || recordingRuntimeActive ? <div className={`screen-action-stack ${activeDevice?.platform ?? "android"}`}>
            {activeDevice?.platform === "android" && <button disabled={!androidReady || !scrcpyReady} onClick={() => onAction("scrcpy")}>
              <MonitorSmartphone />
              <span><strong>键鼠控制</strong><small>{!androidReady ? "需要在线 Android 设备" : scrcpyReady ? "需要时打开交互窗口" : "需要配置 scrcpy"}</small></span>
              <ChevronRight />
            </button>}
            <button disabled={!screenReady || !!mediaBusy} onClick={() => void captureScreenshot(true)}>
              {mediaBusy === "clipboard" ? <LoaderCircle className="spin" /> : <Clipboard />}
              <span><strong>截图到剪贴板</strong><small>{mediaBusy === "clipboard" ? "正在获取当前画面…" : screenReady ? "点击即获取，可直接粘贴" : "屏幕服务就绪后可用"}</small></span>
              <ChevronRight />
            </button>
            <button disabled={!screenReady || !!mediaBusy} onClick={() => void captureScreenshot(false)}>
              {mediaBusy === "screenshot" ? <LoaderCircle className="spin" /> : <Camera />}
              <span><strong>截图保存到电脑</strong><small>{mediaBusy === "screenshot" ? "正在保存 PNG…" : screenReady ? mediaDirectory || "首次使用时选择保存目录" : "屏幕服务就绪后可用"}</small></span>
              <ChevronRight />
            </button>
            {(activeDevice?.platform === "android" || recordingRuntimeActive) && <button disabled={!!recordingBusy || (recordingState.phase === "idle" && (!androidReady || !!mediaBusy))} onClick={() => void (recordingState.phase === "recording" ? stopRecording() : startRecording())}>
              {recordingBusy ? <LoaderCircle className="spin" /> : recordingSession ? <Pause /> : <Video />}
              <span><strong>{recordingBusy === "stopping" ? "正在停止并保存…" : recordingBusy === "starting" ? "正在启动录屏…" : recordingSession ? "停止录屏" : "开始录屏"}</strong><small>{recordingBusy ? `${recordingDeviceName ?? "已锁定设备"} · 请保持连接…` : recordingSession ? `${recordingSession.deviceName} · 已录制 ${recordingTime(recordingElapsed)} · 点击结束并保存` : mediaDirectory || "首次使用时选择保存目录"}</small></span>
              {recordingSession ? <StatusBadge tone="warning">REC {recordingTime(recordingElapsed)}</StatusBadge> : <ChevronRight />}
            </button>}
          </div> : <div className="screen-action-empty"><MonitorSmartphone size={26} /><strong>屏幕操作将在连接后启用</strong><span>Android 会自动启动内嵌 scrcpy；已配对 iOS 会显示屏幕采样。</span></div>}
          <div className="connection-dock">
            <div className="connection-dock-heading"><span><strong>连接设备</strong><small>选择后仍回到当前工作台</small></span><button type="button" onClick={() => setDeviceManagerOpen(true)}>{devices.length ? `管理 ${devices.length} 台` : "设备管理"}</button></div>
            <div className="connection-shortcuts">
              <button className="primary" type="button" onClick={() => onAction("scan")}><Wifi size={16} /><span><strong>自动发现</strong><small>当前局域网 5555</small></span></button>
              <button type="button" onClick={() => onAction("pair")}><ShieldCheck size={16} /><span><strong>无线配对</strong><small>Android 11+</small></span></button>
              <button type="button" onClick={() => onAction("connect")}><Cable size={16} /><span><strong>手动地址</strong><small>ADB IP:端口</small></span></button>
              <button type="button" onClick={() => onAction("ios")}><KeyRound size={16} /><span><strong>iOS SSH</strong><small>越狱设备</small></span></button>
            </div>
          </div>
        </div>
      </div>
    </Panel>
  );

  return (
    <div className="page devices-page">
      <div className="workspace-heading">
        <div><span className="eyebrow">DEVICE WORKBENCH</span><h1>设备工作台</h1><p>实时屏幕、截图录屏与设备连接集中在一个界面。</p></div>
        <Button variant="ghost" icon={<RefreshCw size={15} className={loading ? "spin" : ""} />} onClick={onRefresh} disabled={loading}>刷新设备</Button>
      </div>

      {screenWorkbench}

      {deviceManagerOpen && <Modal title="设备管理" subtitle="选择当前设备，或管理已登记的 iOS SSH 端点" width={940} onClose={() => setDeviceManagerOpen(false)} footer={<><Button onClick={() => { setDeviceManagerOpen(false); onAction("scan"); }}>添加 / 连接设备</Button><Button variant="primary" onClick={() => setDeviceManagerOpen(false)}>完成</Button></>}>
        <div className="table-toolbar device-manager-toolbar">
          <Tabs value={filter} onChange={setFilter} options={[{ id: "all", label: `全部 ${devices.length}` }, { id: "android", label: "Android" }, { id: "ios", label: "iOS" }]} />
          <div className="search-input"><Search size={15} /><input aria-label="搜索设备" value={search} onChange={(e) => setSearch(e.target.value)} placeholder="搜索名称、序列号或 IP" /></div>
        </div>
        {filtered.length ? <div className="data-table device-table device-manager-table">
          <div className="data-row data-head"><span>状态 / 设备</span><span>系统 / 型号</span><span>连接</span><span>权限 / 管理</span></div>
          {filtered.map((device) => {
            const stateLabel = device.state === "online" ? "在线" : device.state === "connecting" ? "连接中" : device.state === "unauthorized" ? "待授权" : device.state === "registered" ? "已登记" : "离线";
            const stateDot = device.state === "online" ? "success" : device.state === "connecting" ? "running" : device.state === "unauthorized" || device.state === "registered" ? "warning" : "muted";
            const accessLabel = device.state === "connecting" ? "连接中" : device.state === "unauthorized" ? "待授权" : device.state !== "online" && device.state !== "registered" ? "离线" : device.rooted ? "Root" : device.jailbroken ? "越狱" : device.platform === "ios" ? "SSH 待验证" : "Shell";
            const accessTone = device.rooted || device.jailbroken ? "purple" : device.state === "connecting" ? "info" : device.state === "unauthorized" || device.state === "registered" ? "warning" : "neutral";
            return <div className={`data-row device-data-row ${activeDevice?.id === device.id ? "selected" : ""} ${device.platform === "ios" && device.connectionSource === "manual" ? "has-management" : ""}`} key={device.id}>
              <button type="button" aria-current={activeDevice?.id === device.id ? "true" : undefined} className="device-row-select" onClick={() => { onSelect(device.id); setDeviceManagerOpen(false); }}>
                <span className="device-table-name"><StatusDot status={stateDot} /><DeviceIdentity device={device} compact showTransport={false} statusLabel={stateLabel} /></span>
                <span><strong>{device.platform === "android" ? "Android" : "iOS"} {device.osVersion}</strong><small>{device.model ?? device.product ?? "未知型号"} · {device.architecture ?? "架构待检测"}</small></span>
                <span><strong className="inline-icon">{device.transport === "wifi" ? <Wifi size={14} /> : <Usb size={14} />}{device.connectionSource === "manual" ? "SSH 端点" : device.transport.toUpperCase()}</strong><small>{device.address ?? "本机直连"}</small></span>
                <span className="device-permission-cell"><StatusBadge tone={accessTone}>{accessLabel}</StatusBadge></span>
              </button>
              {device.platform === "ios" && device.connectionSource === "manual" && <button type="button" className="icon-button danger-icon device-row-manage" title="忘记这个 SSH 端点" aria-label={`忘记 ${device.name} SSH 端点`} onClick={() => onForgetRegisteredIos(device.id)}><Trash2 size={14} /></button>}
            </div>;
          })}
        </div> : <EmptyState icon={<Smartphone size={28} />} title={devices.length ? "没有匹配的设备" : "还没有可用设备"} detail={devices.length ? "请更改筛选条件或搜索内容。" : "使用工作台中的连接入口添加 Android 或 iOS 设备。"} />}
      </Modal>}
    </div>
  );
}
