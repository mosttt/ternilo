import { describe, expect, it, vi } from 'vitest'
import {
  defaultPermissionStorageKey,
  permissionForPlacement,
  permissionPresetsForPlacement,
  readDefaultPermission,
  writeDefaultPermission,
} from './default-permission'

describe('new session permission preference', () => {
  it('defaults invalid or absent values to workspace write', () => {
    expect(readDefaultPermission({ getItem: () => null })).toBe('workspace_write')
    expect(readDefaultPermission({ getItem: () => 'root' })).toBe('workspace_write')
  })

  it('persists a typed permission preset', () => {
    const setItem = vi.fn()
    writeDefaultPermission({ setItem }, 'read_only')
    expect(setItem).toHaveBeenCalledWith(defaultPermissionStorageKey, 'read_only')
  })

  it('keeps full access local and maps the cloud default to workspace write', () => {
    expect(permissionForPlacement('full_access', 'local_node')).toBe('full_access')
    expect(permissionForPlacement('full_access', 'cloud')).toBe('workspace_write')
    expect(permissionForPlacement('read_only', 'cloud')).toBe('read_only')
    expect(permissionPresetsForPlacement('local_node')).toContain('full_access')
    expect(permissionPresetsForPlacement('cloud')).toEqual(['read_only', 'workspace_write'])
  })
})
