import { isTauri } from "./tauri";
import { getCurrentWebview } from "@tauri-apps/api/webview";

export const ZOOM_LEVELS = [0.5, 0.6, 0.7, 0.75, 0.8, 0.85, 0.9, 0.95, 1.0, 1.1, 1.2, 1.3, 1.4, 1.5, 1.75, 2.0];
export const ZOOM_DEFAULT = 1.0;
const ZOOM_STORAGE_KEY = "hyvemind:zoom-level";

export function getStoredZoom(): number {
  try {
    const raw = localStorage.getItem(ZOOM_STORAGE_KEY);
    if (raw == null) return ZOOM_DEFAULT;
    const n = parseFloat(raw);
    if (!Number.isFinite(n) || n < ZOOM_LEVELS[0] || n > ZOOM_LEVELS[ZOOM_LEVELS.length - 1]) return ZOOM_DEFAULT;
    return n;
  } catch { return ZOOM_DEFAULT; }
}

export function saveZoom(level: number): void {
  try { localStorage.setItem(ZOOM_STORAGE_KEY, String(level)); } catch {}
}

export async function applyZoom(level: number): Promise<void> {
  if (!isTauri()) return;
  try {
    await getCurrentWebview().setZoom(level);
  } catch (e) {
    console.error("Failed to set zoom:", e);
  }
}

export function zoomIn(current: number): number {
  for (const l of ZOOM_LEVELS) { if (l > current + 0.001) return l; }
  return ZOOM_LEVELS[ZOOM_LEVELS.length - 1];
}

export function zoomOut(current: number): number {
  for (let i = ZOOM_LEVELS.length - 1; i >= 0; i--) {
    if (ZOOM_LEVELS[i] < current - 0.001) return ZOOM_LEVELS[i];
  }
  return ZOOM_LEVELS[0];
}

export function zoomReset(): number { return ZOOM_DEFAULT; }

export function formatZoomPercent(level: number): string {
  return `${Math.round(level * 100)}%`;
}
