/// The sidebar's width in CSS pixels before any drag, and after a double-click on its edge.
export const SIDEBAR_DEFAULT = 264;
export const SIDEBAR_MIN = 180;

/// `width` rounded and kept between `SIDEBAR_MIN` and half the window; the minimum wins in a window too narrow for
/// both. The stylesheet clamps the same way, so a window shrunk after a drag still gets the bounds.
export function clampSidebarWidth(width: number, windowWidth: number): number {
  if (!Number.isFinite(width)) return SIDEBAR_DEFAULT;
  return Math.round(Math.max(SIDEBAR_MIN, Math.min(width, windowWidth / 2)));
}
