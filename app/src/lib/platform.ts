import { isTauri } from "./tauri";

export { isTauri };

/**
 * Mirror of the Tauri window builder `traffic_light_position` (lib.rs).
 * @deprecated Use `MAC_TRAFFIC_LIGHT_GUTTER_PX` for frontend layout.
 *   This value exists for documentation / reference only — actual button
 *   positioning is controlled by the Rust builder call.
 */
export const MAC_TRAFFIC_LIGHT_POSITION = { x: 18, y: 25 } as const;

/**
 * Nominal gutter reserved left of the header for the macOS traffic-light
 * button group. Derived from `traffic_light_position.x` (14 at the Tauri
 * level) + the button-group width (~52px for 3 × 12px buttons + 2 × 8px
 * gaps) + visual padding.
 *
 * **Invariant:** `GUTTER_PX − traffic_light_position.x = 70`. The 70px
 * of drag-usable area to the right of the buttons MUST stay constant.
 * When `traffic_light_position.x` changes, this value MUST change by the
 * same delta.
 */
export const MAC_TRAFFIC_LIGHT_GUTTER_PX = 84;

/**
 * Cosmetic frontend platform check.
 *
 * Use this with isTauri() before applying native-window chrome spacing.
 * It is intentionally synchronous and does not replace Tauri config/platform
 * validation.
 */
export const isMac = (): boolean => {
  if (typeof navigator === "undefined") return false;

  const uaDataPlatform = (navigator as any).userAgentData?.platform || "";
  const userAgent = navigator.userAgent || "";
  const platform = navigator.platform || "";

  return (
    /^macOS$/i.test(uaDataPlatform) ||
    /Macintosh|Mac OS X/.test(userAgent) ||
    /^Mac/.test(platform)
  );
};
