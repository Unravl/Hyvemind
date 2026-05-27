import { confirm as tauriConfirm } from "@tauri-apps/plugin-dialog";
import { isTauri } from "./tauri";

export interface ConfirmDialogOptions {
  title?: string;
  okLabel?: string;
  cancelLabel?: string;
  kind?: "info" | "warning" | "error";
}

/**
 * Show a confirmation dialog and return whether the user confirmed.
 *
 * In a Tauri context, this uses the OS-native confirm dialog via
 * `@tauri-apps/plugin-dialog`. Outside Tauri (vitest/jsdom, plain browser
 * during `npm run dev`), it falls back to `window.confirm` so tests and the
 * dev workflow keep working.
 *
 * On any IPC / permission error the function returns `false` (default-deny),
 * which is the safe behaviour for destructive actions.
 */
export async function confirmDialog(
  message: string,
  opts: ConfirmDialogOptions = {},
): Promise<boolean> {
  const { title, okLabel, cancelLabel, kind = "warning" } = opts;

  if (isTauri()) {
    try {
      return await tauriConfirm(message, {
        title,
        kind,
        okLabel,
        cancelLabel,
      });
    } catch (err) {
      console.error("confirmDialog: tauri confirm failed:", err);
      return false;
    }
  }

  if (typeof window !== "undefined" && typeof window.confirm === "function") {
    return window.confirm(message);
  }

  return false;
}
