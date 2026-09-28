import type { PermissionPreset } from '@/types'

export const defaultPermissionStorageKey = 'ternilo.default-permission'

const permissionPresets: readonly PermissionPreset[] = ['read_only', 'workspace_write', 'full_access']
const cloudPermissionPresets: readonly PermissionPreset[] = ['read_only', 'workspace_write']

export type SessionPlacement = 'local_node' | 'cloud'

export function readDefaultPermission(storage: Pick<Storage, 'getItem'>): PermissionPreset {
  const value = storage.getItem(defaultPermissionStorageKey)
  return permissionPresets.includes(value as PermissionPreset) ? value as PermissionPreset : 'workspace_write'
}

export function writeDefaultPermission(storage: Pick<Storage, 'setItem'>, value: PermissionPreset) {
  storage.setItem(defaultPermissionStorageKey, value)
}

export function permissionPresetsForPlacement(placement?: SessionPlacement): readonly PermissionPreset[] {
  return placement === 'cloud' ? cloudPermissionPresets : permissionPresets
}

export function permissionForPlacement(permission: PermissionPreset, placement?: SessionPlacement): PermissionPreset {
  return placement === 'cloud' && permission === 'full_access' ? 'workspace_write' : permission
}
