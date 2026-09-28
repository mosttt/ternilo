import { describe, expect, it } from 'vitest'
import '@/components/settings/builtin-settings'
import { settingsSectionRegistry, visibleSettingsSections } from './settings-registry'

describe('settings section contributions', () => {
  it('keeps shared resources separate from machine configuration ownership', () => {
    expect(visibleSettingsSections({ platform: true, tenantRole: 'member', targetOwner: false }).map(item => item.id))
      .toEqual(['general', 'computers', 'about'])
    expect(visibleSettingsSections({ platform: true, tenantRole: 'member', targetOwner: true }).map(item => item.id))
      .toEqual(['general', 'plugins', 'presets', 'credentials', 'computers', 'about'])
  })
  it('keeps instance mode separate from the space administrator role', () => {
    const sections = (instanceOwner: boolean, tenantRole: 'admin' | 'owner') => visibleSettingsSections({
      platform: true, tenantRole, instanceOwner,
    }).map(item => item.id)
    expect(sections(false, 'owner')).not.toContain('instance')
    expect(sections(false, 'admin')).not.toContain('instance')
    expect(sections(true, 'owner')).not.toContain('instance')
    expect(sections(true, 'owner')).toContain('computers')
  })
  it('keeps platform administration privileged and exposes self-service computers to operators', () => {
    expect(visibleSettingsSections({ platform: false, tenantRole: null }).map(item => item.id)).toEqual([
      'general', 'models', 'plugins', 'presets', 'credentials', 'about',
    ])
    expect(visibleSettingsSections({ platform: true, tenantRole: 'owner' }).map(item => item.id)).toEqual([
      'general', 'plugins', 'presets', 'credentials', 'computers', 'about',
    ])
    expect(visibleSettingsSections({ platform: true, tenantRole: 'member' }).map(item => item.id)).toEqual([
      'general', 'plugins', 'presets', 'credentials', 'computers', 'about',
    ])
    expect(visibleSettingsSections({ platform: true, tenantRole: 'viewer' }).map(item => item.id)).toEqual([
      'general', 'plugins', 'presets', 'credentials', 'about',
    ])
    expect(settingsSectionRegistry.getSnapshot()).toBe(settingsSectionRegistry.getSnapshot())
  })
})
