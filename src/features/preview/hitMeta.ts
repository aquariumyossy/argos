import type { SearchHit } from "./types";

export function isOutlookHit(hit: SearchHit): boolean {
  return (
    hit.source === "outlook" ||
    hit.docKind === "email" ||
    hit.path.startsWith("outlook:")
  );
}

export function parentDir(path: string): string | null {
  if (path.startsWith("outlook:") || path.startsWith("mailfolder:")) {
    return null;
  }
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const i = normalized.lastIndexOf("\\");
  if (i <= 0) return null;
  const parent = normalized.slice(0, i);
  if (/^[A-Za-z]:$/.test(parent)) return `${parent}\\`;
  if (!parent || parent === "\\") return null;
  return parent;
}

function splitMailPathLabel(pathLabel: string): { store: string; folderParts: string[] } {
  const parts = pathLabel
    .split("/")
    .map((s) => s.trim())
    .filter(Boolean);
  if (parts.length === 0) return { store: "", folderParts: [] };
  const store = parts[0];
  let i = 1;
  while (
    i < parts.length &&
    parts[i]!.localeCompare(store, undefined, { sensitivity: "accent" }) === 0
  ) {
    i += 1;
  }
  return { store, folderParts: parts.slice(i) };
}

export function formatMailScopeLabel(pathLabel: string): string {
  const { store, folderParts } = splitMailPathLabel(pathLabel);
  if (!store) return "メール";
  if (folderParts.length === 0) return `メール：${store}`;
  return `メール：${folderParts.join("／")}（${store}）`;
}

export function formatMailFolderMeta(pathLabel: string): string {
  const { store, folderParts } = splitMailPathLabel(pathLabel);
  if (!store) return pathLabel.trim();
  if (folderParts.length === 0) return store;
  return `${folderParts.join("／")}（${store}）`;
}

export function formatMailDateYmd(unixStr?: string): string {
  if (!unixStr) return "";
  const n = Number(unixStr);
  if (!Number.isFinite(n) || n <= 0) return "";
  const d = new Date(n * 1000);
  if (Number.isNaN(d.getTime())) return "";
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}/${m}/${day}`;
}

export function scopeChipLabel(path: string, label?: string | null): string {
  if (label && label.trim()) return label.trim();
  if (path.startsWith("mailfolder:")) {
    return formatMailScopeLabel(path.slice("mailfolder:".length));
  }
  if (path.startsWith("outlook:")) {
    return "Outlook メール";
  }
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  const base = normalized.split("\\").filter(Boolean).pop();
  return base || path;
}

export function fileBaseName(path: string): string {
  const normalized = path.replace(/\//g, "\\").replace(/\\+$/, "");
  return normalized.split("\\").filter(Boolean).pop() || path;
}
