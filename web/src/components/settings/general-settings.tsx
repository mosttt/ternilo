import { AccountSettings } from './account-settings'
import * as React from 'react'
import { api } from '@/api/client'
import { Select } from '@/components/ui/field'
import { applyThemePreference, type ThemePreference } from '@/domain/theme'
import { readBusyEnterBehavior, writeBusyEnterBehavior } from '@/domain/composer-preference'
import {
  contentFontSizes,
  readContentFontSize,
  type ContentFontSize,
  writeContentFontSize,
} from '@/domain/content-font'
import { useTranscriptView, writeTranscriptView } from '@/domain/transcript-view'
import { ownsExecutionConfiguration } from '@/domain/resource-access'
import { readDefaultPermission, writeDefaultPermission } from '@/domain/default-permission'
import { localizeAgentPreset } from '@/i18n/builtin-metadata'
import { executionTargetPath, type ExecutionTarget } from '@/domain/execution-target'
import { useLocale, useTranslate } from '@/i18n/provider'
import { storage, useWorkbench } from '@/state/workbench'
import type { PermissionPreset } from '@/types'
import { ActionDialog, SectionHeader, SettingRow } from './settings-ui'
import styles from './settings-layout.module.css'

const contentFontLabels: Record<ContentFontSize, 'general.contentFont14' | 'general.contentFont16' | 'general.contentFont18'> = {
  14: 'general.contentFont14',
  16: 'general.contentFont16',
  18: 'general.contentFont18',
}

export function GeneralSettings() {
  const { platform, presets, currentSession, currentWorkspace, refresh, notify } = useWorkbench()
  const targetOwner = ownsExecutionConfiguration(currentWorkspace, currentSession, true)
  const { locale, setLocale } = useLocale()
  const t = useTranslate('settings')
  const builtins = useTranslate('builtins')
  const conversation = useTranslate('conversation')
  const [theme, setTheme] = React.useState(() => localStorage.getItem(storage.theme) ?? 'system')
  const transcriptView = useTranscriptView()
  const [busyEnter, setBusyEnter] = React.useState(() => readBusyEnterBehavior(localStorage))
  const [contentFontSize, setContentFontSize] = React.useState(() => readContentFontSize(localStorage))
  const [defaultPermission, setDefaultPermission] = React.useState(() => readDefaultPermission(localStorage))
  const [fullAccessOpen, setFullAccessOpen] = React.useState(false)
  const [permissionBusy, setPermissionBusy] = React.useState(false)
  const [permissionError, setPermissionError] = React.useState('')
  const target = React.useMemo<ExecutionTarget>(() => ({
    sessionId: currentSession?.identity.session_id,
    workspaceId: currentWorkspace?.workspace_id,
  }), [currentSession?.identity.session_id, currentWorkspace?.workspace_id])

  const applyTheme = (next: string) => {
    setTheme(next)
    localStorage.setItem(storage.theme, next)
    applyThemePreference(next as ThemePreference)
  }

  const grantFullAccess = async () => {
    setPermissionBusy(true)
    setPermissionError('')
    try {
      writeDefaultPermission(localStorage, 'full_access')
      setDefaultPermission('full_access')
      setFullAccessOpen(false)
    } catch (cause) {
      setPermissionError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setPermissionBusy(false)
    }
  }

  const setDefaultPreset = async (id: string) => {
    if (!targetOwner || id === presets.default_id) return
    try {
      await api.request(executionTargetPath(`/agent-presets/${encodeURIComponent(id)}/default`, target), { method: 'PUT' })
      await refresh()
      notify(t('presets.defaultUpdated'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }

  return (
    <div className={styles.section}>
      <SectionHeader title={t('nav.general')} description={t('general.description')} />
      <section className="rounded-xl border bg-card px-4">
        <SettingRow title={t('general.language')}>
          <Select
            aria-label={t('general.language')}
            className="w-40"
            value={locale}
            onValueChange={(nextValue) => setLocale(nextValue === 'en' ? 'en' : 'zh')}
          >
            <option value="zh">中文</option>
            <option value="en">English</option>
          </Select>
        </SettingRow>
        <SettingRow title={t('general.theme')} description={t('general.themeDescription')}>
          <Select
            aria-label={t('general.theme')}
            className="w-40"
            value={theme}
            onValueChange={(nextValue) => applyTheme(nextValue)}
          >
            <option value="light">{t('general.themeLight')}</option>
            <option value="dark">{t('general.themeDark')}</option>
            <option value="system">{t('general.themeSystem')}</option>
          </Select>
        </SettingRow>
        <SettingRow title={t('general.contentFont')} description={t('general.contentFontDescription')}>
          <Select
            aria-label={t('general.contentFont')}
            className="w-40"
            value={contentFontSize}
            onValueChange={(nextValue) => {
              const next = Number(nextValue) as ContentFontSize
              setContentFontSize(next)
              writeContentFontSize(localStorage, next)
            }}
          >
            {contentFontSizes.map((size) => (
              <option value={size} key={size}>{t(contentFontLabels[size])}</option>
            ))}
          </Select>
        </SettingRow>
        <SettingRow title={t('general.transcript')} description={t('general.transcriptDescription')}>
          <Select
            aria-label={t('general.transcriptAria')}
            className="w-40"
            value={transcriptView}
            onValueChange={(nextValue) => writeTranscriptView(localStorage, nextValue as 'normal' | 'compact')}
          >
            <option value="compact">{t('general.compact')}</option>
            <option value="normal">{t('general.full')}</option>
          </Select>
        </SettingRow>
        <SettingRow title={t('general.busyEnter')} description={t('general.busyEnterDescription')}>
          <Select
            aria-label={t('general.busyEnter')}
            className="w-40"
            value={busyEnter}
            onValueChange={(nextValue) => {
              const value = nextValue === 'steer' ? 'steer' : 'queue'
              setBusyEnter(value)
              writeBusyEnterBehavior(localStorage, value)
            }}
          >
            <option value="queue">{t('general.busyEnterQueue')}</option>
            <option value="steer">{t('general.busyEnterSteer')}</option>
          </Select>
        </SettingRow>
        <SettingRow
          title={t('general.permission')}
          description={t(platform ? 'general.permissionPlatformDescription' : 'general.permissionDescription')}
        >
          <Select
            aria-label={t('general.permission')}
            className="w-48"
            value={defaultPermission}
            onValueChange={(nextValue) => {
              const value = nextValue as PermissionPreset
              if (value === 'full_access') {
                setPermissionError('')
                setFullAccessOpen(true)
                return
              }
              writeDefaultPermission(localStorage, value)
              setDefaultPermission(value)
            }}
          >
            <option value="read_only">{conversation('permission.readOnly')}</option>
            <option value="workspace_write">{conversation('permission.workspaceWrite')}</option>
            <option value="full_access">
              {platform ? t('general.fullAccessLocalOnly') : conversation('permission.fullAccess')}
            </option>
          </Select>
        </SettingRow>
        <SettingRow title={t('general.preset')} description={t('general.presetDescription')}>
          <Select
            aria-label={t('general.preset')}
            className="w-52"
            value={presets.default_id}
            disabled={!targetOwner || presets.presets.length === 0}
            onValueChange={(nextValue) => void setDefaultPreset(nextValue)}
          >
            {presets.presets.map((sourcePreset) => {
              const preset = localizeAgentPreset(sourcePreset, builtins)
              return <option key={preset.id} value={preset.id}>
                {preset.display_name}{preset.trust === 'user' ? ` · ${t('presets.custom')}` : ''}
              </option>
            })}
          </Select>
        </SettingRow>
      </section>
      {platform && <div className="mt-6"><AccountSettings /></div>}
      <ActionDialog
        open={fullAccessOpen}
        title={t('general.fullAccessTitle')}
        description={t('general.fullAccessDescription')}
        cancelLabel={conversation('permission.workspaceWrite')}
        confirmLabel={t('general.fullAccessApply')}
        busyLabel={t('general.fullAccessApplying')}
        busy={permissionBusy}
        destructive
        error={permissionError}
        onOpenChange={setFullAccessOpen}
        onConfirm={() => void grantFullAccess()}
      />
    </div>
  )
}
