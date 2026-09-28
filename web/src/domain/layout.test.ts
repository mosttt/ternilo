import { describe, expect, it } from 'vitest'
import {
  CENTER_MIN,
  computeColumns,
  DEFAULT_LAYOUT_PREFERENCES,
  DETAILS_DEFAULT,
  DETAILS_MAX,
  DETAILS_MIN,
  normalizeLayoutPreferences,
  SIDEBAR_COLLAPSED,
  SIDEBAR_DEFAULT,
  SIDEBAR_MAX,
  SIDEBAR_MIN,
} from './layout'

describe('computeColumns', () => {
  it('gives files a half-width workspace without widening the event inspector', () => {
    expect(computeColumns(1500, 280, 610, 1050, 440)).toEqual({ sidebar: 280, center: 610, details: 610 })
    expect(computeColumns(1500, 280, 610).details).toBe(DETAILS_MAX)
    expect(computeColumns(900, 0, 610, 630, 440)).toEqual({ sidebar: 56, center: 440, details: 404 })
    expect(computeColumns(780, 0, 610, 546, 440).details).toBe(0)
  })
  it('keeps all preferred widths when the frame has room', () => {
    expect(computeColumns(1920, SIDEBAR_DEFAULT, DETAILS_DEFAULT)).toEqual({
      sidebar: 280,
      center: 1280,
      details: 360,
    })
  })

  it('uses a fixed rail for a closed sidebar and zero for closed details', () => {
    expect(computeColumns(1920, 0, 0)).toEqual({
      sidebar: SIDEBAR_COLLAPSED,
      center: 1920 - SIDEBAR_COLLAPSED,
      details: 0,
    })
  })

  it('clamps preferences at the domain boundary', () => {
    expect(computeColumns(1920, 1, 1)).toMatchObject({ sidebar: SIDEBAR_MIN, details: DETAILS_MIN })
    expect(computeColumns(2400, 9999, 9999)).toMatchObject({ sidebar: SIDEBAR_MAX, details: DETAILS_MAX })
  })

  it('shrinks details before closing it while preserving the center floor', () => {
    expect(computeColumns(1250, SIDEBAR_DEFAULT, DETAILS_DEFAULT)).toEqual({
      sidebar: SIDEBAR_DEFAULT,
      center: CENTER_MIN,
      details: 330,
    })
    expect(computeColumns(1220, SIDEBAR_DEFAULT, DETAILS_DEFAULT)).toEqual({
      sidebar: SIDEBAR_DEFAULT,
      center: CENTER_MIN,
      details: DETAILS_MIN,
    })
  })

  it('derives details closed once its minimum would starve the center', () => {
    expect(computeColumns(1219, SIDEBAR_DEFAULT, DETAILS_DEFAULT)).toEqual({
      sidebar: SIDEBAR_DEFAULT,
      center: 939,
      details: 0,
    })
  })

  it('lets the center absorb the final deficit without shrinking the sidebar', () => {
    expect(computeColumns(700, SIDEBAR_DEFAULT, DETAILS_DEFAULT)).toEqual({
      sidebar: SIDEBAR_DEFAULT,
      center: 420,
      details: 0,
    })
  })

  it('recovers preferred details width after the frame grows', () => {
    expect(computeColumns(1100, SIDEBAR_DEFAULT, DETAILS_DEFAULT).details).toBe(0)
    expect(computeColumns(1920, SIDEBAR_DEFAULT, DETAILS_DEFAULT).details).toBe(DETAILS_DEFAULT)
  })
})

describe('normalizeLayoutPreferences', () => {
  it('supplies defaults and clamps persisted values', () => {
    expect(normalizeLayoutPreferences(undefined)).toEqual(DEFAULT_LAYOUT_PREFERENCES)
    expect(normalizeLayoutPreferences({ sidebarWidth: 1, detailsWidth: 9999, sidebarCollapsed: true })).toEqual({
      sidebarWidth: SIDEBAR_MIN,
      detailsWidth: DETAILS_MAX,
      sidebarCollapsed: true,
    })
  })
})
