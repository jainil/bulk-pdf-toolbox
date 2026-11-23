export function fileName(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(i + 1) : p;
}

export function dirName(p: string): string {
  const i = Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\"));
  return i >= 0 ? p.slice(0, i) : "";
}

/** Shorten /Users/name/... (or C:\\Users\\name\\...) to ~/... for display. */
export function tildify(p: string): string {
  const m = p.match(/^\/Users\/[^/]+/) ?? p.match(/^[A-Za-z]:\\Users\\[^\\]+/);
  return m ? "~" + p.slice(m[0].length) : p;
}

export const isWindows = typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);
export const FILE_MANAGER = isWindows ? "File Explorer" : "Finder";
export const TRASH = isWindows ? "Recycle Bin" : "Trash";

export function bytes(n: number | null | undefined): string {
  if (n == null) return "—";
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function duration(ms: number): string {
  const s = Math.round(ms / 1000);
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, "0")}s`;
  const h = Math.floor(m / 60);
  return `${h}h ${String(m % 60).padStart(2, "0")}m`;
}

export function plural(n: number, word: string): string {
  return `${n.toLocaleString()} ${word}${n === 1 ? "" : "s"}`;
}
