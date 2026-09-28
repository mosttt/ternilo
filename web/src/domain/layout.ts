/**
 * Three-column workbench geometry.
 *
 * Preferences are deliberately separate from resolved widths: when the
 * details column concedes space, its preferred width is left untouched so it
 * can recover automatically when the frame grows again.
 */

export interface WorkbenchColumns {
  sidebar: number
  center: number
  details: number
}

export interface LayoutPreferences {
  sidebarWidth: number
  detailsWidth: number
  sidebarCollapsed: boolean
}

export const CENTER_MIN = 640
export const SIDEBAR_MIN = 264
export const SIDEBAR_MAX = 420
export const SIDEBAR_DEFAULT = 280
export const SIDEBAR_COLLAPSED = 56
export const SIDEBAR_AUTO_COLLAPSE = 1024
export const MOBILE_BREAKPOINT = 760
export const DETAILS_MIN = 300
export const DETAILS_MAX = 520
export const DETAILS_DEFAULT = 360

export const DEFAULT_LAYOUT_PREFERENCES: LayoutPreferences = {
  sidebarWidth: SIDEBAR_DEFAULT,
  detailsWidth: DETAILS_DEFAULT,
  sidebarCollapsed: false,
}

export function clampWidth(px: number, min: number, max: number): number {
  if (!Number.isFinite(px)) return min
  return Math.min(max, Math.max(min, Math.round(px)))
}

/**
 * Resolve the visible desktop tracks. A zero sidebar preference means the
 * compact control rail; a zero details preference means a mounted, zero-width
 * details column.
 */
export function computeColumns(viewport: number, sidebar: number, details: number, detailsMax = DETAILS_MAX, centerMin = CENTER_MIN): WorkbenchColumns {
  const available = Math.max(0, Math.round(viewport))
  const sidebarWidth = sidebar === 0
    ? SIDEBAR_COLLAPSED
    : clampWidth(sidebar, SIDEBAR_MIN, SIDEBAR_MAX)
  const preferredDetails = details === 0
    ? 0
    : clampWidth(details, DETAILS_MIN, detailsMax)

  if (sidebarWidth + centerMin + preferredDetails <= available) {
    return {
      sidebar: sidebarWidth,
      center: available - sidebarWidth - preferredDetails,
      details: preferredDetails,
    }
  }

  const concededDetails = preferredDetails === 0
    ? 0
    : Math.max(DETAILS_MIN, available - sidebarWidth - centerMin)
  if (sidebarWidth + centerMin + concededDetails <= available) {
    return { sidebar: sidebarWidth, center: centerMin, details: concededDetails }
  }

  return {
    sidebar: sidebarWidth,
    center: Math.max(0, available - sidebarWidth),
    details: 0,
  }
}

export function normalizeLayoutPreferences(value: Partial<LayoutPreferences> | null | undefined): LayoutPreferences {
  return {
    sidebarWidth: clampWidth(value?.sidebarWidth ?? SIDEBAR_DEFAULT, SIDEBAR_MIN, SIDEBAR_MAX),
    detailsWidth: clampWidth(value?.detailsWidth ?? DETAILS_DEFAULT, DETAILS_MIN, DETAILS_MAX),
    sidebarCollapsed: value?.sidebarCollapsed === true,
  }
}
