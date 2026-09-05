const isDesktop = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export interface SelectedPackageFile {
  path: string;
  name: string;
  size?: number;
  browserFile?: File;
}

export function packageSelectionFromFile(file: File): SelectedPackageFile {
  return {
    path: `browser-preview://${encodeURIComponent(file.name)}`,
    name: file.name,
    size: file.size,
    browserFile: file,
  };
}

function browserFilePicker(): Promise<SelectedPackageFile | null> {
  return new Promise((resolve) => {
    const input = document.createElement("input");
    let settled = false;
    const finish = (selection: SelectedPackageFile | null) => {
      if (settled) return;
      settled = true;
      window.removeEventListener("focus", handleWindowFocus);
      input.remove();
      resolve(selection);
    };
    const handleWindowFocus = () => {
      // Some browsers do not emit the non-standard `cancel` event. Waiting one
      // tick lets a pending `change` event win before treating the picker as closed.
      window.setTimeout(() => {
        const file = input.files?.[0];
        finish(file ? packageSelectionFromFile(file) : null);
      }, 250);
    };
    input.type = "file";
    input.accept = ".apk,.ipa,application/vnd.android.package-archive,application/octet-stream";
    input.style.position = "fixed";
    input.style.left = "-10000px";
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      finish(file ? packageSelectionFromFile(file) : null);
    }, { once: true });
    input.addEventListener("cancel", () => finish(null), { once: true });
    document.body.appendChild(input);
    window.addEventListener("focus", handleWindowFocus, { once: true });
    input.click();
  });
}

export async function choosePackageFile(): Promise<SelectedPackageFile | null> {
  if (!isDesktop()) return browserFilePicker();
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({
    multiple: false,
    directory: false,
    title: "选择 Android APK 或 iOS IPA",
    filters: [{ name: "移动应用包", extensions: ["apk", "ipa"] }],
  });
  if (typeof selected !== "string") return null;
  return { path: selected, name: selected.split(/[\\/]/).pop() ?? selected };
}

export async function chooseBinaryFile(title: string): Promise<string | null> {
  if (!isDesktop()) return "/Users/demo/Downloads/mobius-runtime";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ multiple: false, directory: false, title });
  return typeof selected === "string" ? selected : null;
}

export async function chooseLocalFile(title: string): Promise<string | null> {
  if (!isDesktop()) return "/Users/demo/Downloads/sample.bin";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ multiple: false, directory: false, title });
  return typeof selected === "string" ? selected : null;
}

export async function chooseDirectory(title: string): Promise<string | null> {
  if (!isDesktop()) return "/Users/demo/Downloads";
  const { open } = await import("@tauri-apps/plugin-dialog");
  const selected = await open({ multiple: false, directory: true, title });
  return typeof selected === "string" ? selected : null;
}
