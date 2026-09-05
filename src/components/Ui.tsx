import { useEffect, useId, useRef } from "react";
import type { ButtonHTMLAttributes, KeyboardEvent as ReactKeyboardEvent, ReactNode } from "react";
import { AlertTriangle, Check, CircleAlert, Info, LoaderCircle, X } from "lucide-react";
import type { Device, ToastMessage } from "../types";
import { maskIdentifier } from "../lib/format";

export function Panel({ title, action, children, className = "" }: { title?: ReactNode; action?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <section className={`panel ${className}`}>
      {(title || action) && (
        <header className="panel-header">
          <div className="panel-title">{title}</div>
          {action && <div className="panel-action">{action}</div>}
        </header>
      )}
      <div className="panel-content">{children}</div>
    </section>
  );
}

export function Button({ variant = "secondary", icon, children, className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "primary" | "secondary" | "ghost" | "danger"; icon?: ReactNode }) {
  return (
    <button className={`button button-${variant} ${className}`} {...props}>
      {icon}
      {children}
    </button>
  );
}

export function StatusDot({ status }: { status: "success" | "warning" | "error" | "running" | "info" | "muted" }) {
  return <span className={`status-dot status-${status}`} aria-hidden="true" />;
}

export function StatusBadge({ tone = "neutral", children }: { tone?: "success" | "warning" | "danger" | "info" | "purple" | "neutral"; children: ReactNode }) {
  return <span className={`badge badge-${tone}`}>{children}</span>;
}

export function DeviceIdentity({ device, compact = false, showTransport = true, statusLabel }: { device?: Device; compact?: boolean; showTransport?: boolean; statusLabel?: string }) {
  if (!device) return <span className="muted">未选择设备</span>;
  return (
    <div className={`device-identity ${compact ? "device-identity-compact" : ""}`}>
      <span className={`platform-orb ${device.platform}`}>{device.platform === "android" ? "A" : "i"}</span>
      <div>
        <strong>{device.name}</strong>
        <span>{maskIdentifier(device.id)}{showTransport && ` · ${device.transport.toUpperCase()}`}{statusLabel && ` · ${statusLabel}`}</span>
      </div>
    </div>
  );
}

export function EmptyState({ icon, title, detail, actions }: { icon: ReactNode; title: string; detail: string; actions?: ReactNode }) {
  return (
    <div className="empty-state">
      <div className="empty-icon">{icon}</div>
      <h3>{title}</h3>
      <p>{detail}</p>
      {actions && <div className="empty-actions">{actions}</div>}
    </div>
  );
}

export function Modal({ title, subtitle, children, footer, onClose, width = 520 }: { title: string; subtitle?: string; children: ReactNode; footer?: ReactNode; onClose: () => void; width?: number }) {
  const dialogRef = useRef<HTMLElement>(null);
  const onCloseRef = useRef(onClose);
  const titleId = useId();
  onCloseRef.current = onClose;
  useEffect(() => {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const dialog = dialogRef.current;
    const focusable = () => Array.from(dialog?.querySelectorAll<HTMLElement>('button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href], [tabindex]:not([tabindex="-1"])') ?? []);
    const preferred = dialog?.querySelector<HTMLElement>('[autofocus], [data-autofocus="true"]');
    (preferred ?? focusable()[0] ?? dialog)?.focus();
    const keydown = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); onCloseRef.current(); return; }
      if (event.key !== "Tab") return;
      const items = focusable();
      if (!items.length) { event.preventDefault(); dialog?.focus(); return; }
      const first = items[0];
      const last = items[items.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
    };
    document.addEventListener("keydown", keydown);
    return () => { document.removeEventListener("keydown", keydown); previous?.focus(); };
  }, []);
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <section ref={dialogRef} className="modal" role="dialog" aria-modal="true" aria-labelledby={titleId} tabIndex={-1} style={{ width }}>
        <header className="modal-header">
          <div>
            <h2 id={titleId}>{title}</h2>
            {subtitle && <p>{subtitle}</p>}
          </div>
          <button className="icon-button" onClick={onClose} aria-label="关闭"><X size={18} /></button>
        </header>
        <div className="modal-body">{children}</div>
        {footer && <footer className="modal-footer">{footer}</footer>}
      </section>
    </div>
  );
}

export function Tabs<T extends string>({ value, options, onChange }: { value: T; options: Array<{ id: T; label: string }>; onChange: (value: T) => void }) {
  const baseId = useId();
  const move = (event: ReactKeyboardEvent<HTMLButtonElement>, index: number) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
    event.preventDefault();
    const nextIndex = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1 : event.key === 'ArrowLeft' ? (index - 1 + options.length) % options.length : (index + 1) % options.length;
    onChange(options[nextIndex].id);
    event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[nextIndex]?.focus();
  };
  return (
    <div className="tabs" role="tablist" aria-label="页面分区">
      {options.map((option, index) => (
        <button key={option.id} id={`${baseId}-tab-${option.id}`} role="tab" aria-selected={value === option.id} tabIndex={value === option.id ? 0 : -1} className={value === option.id ? "active" : ""} onKeyDown={(event) => move(event, index)} onClick={() => onChange(option.id)}>
          {option.label}
        </button>
      ))}
    </div>
  );
}

export function InlineNotice({ tone = "info", title, children }: { tone?: "info" | "warning" | "danger" | "success"; title: string; children: ReactNode }) {
  const Icon = tone === "warning" || tone === "danger" ? AlertTriangle : tone === "success" ? Check : Info;
  return (
    <div className={`inline-notice notice-${tone}`}>
      <Icon size={17} />
      <div><strong>{title}</strong><span>{children}</span></div>
    </div>
  );
}

export function ToastStack({ messages, dismiss }: { messages: ToastMessage[]; dismiss: (id: number) => void }) {
  const icons = {
    success: <Check size={17} />,
    error: <CircleAlert size={17} />,
    warning: <AlertTriangle size={17} />,
    info: <Info size={17} />,
  };
  return (
    <div className="toast-stack" aria-live="polite">
      {messages.map((message) => (
        <div key={message.id} className={`toast toast-${message.type}`}>
          <span className="toast-icon">{icons[message.type]}</span>
          <div><strong>{message.title}</strong>{message.detail && <span>{message.detail}</span>}</div>
          <button className="icon-button" onClick={() => dismiss(message.id)} aria-label="关闭通知"><X size={15} /></button>
        </div>
      ))}
    </div>
  );
}

export function BusyLabel({ children = "处理中…" }: { children?: ReactNode }) {
  return <span className="busy-label"><LoaderCircle className="spin" size={15} />{children}</span>;
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="field">
      <span className="field-label">{label}</span>
      {children}
      {hint && <span className="field-hint">{hint}</span>}
    </label>
  );
}
