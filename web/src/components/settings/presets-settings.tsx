import * as React from 'react'
import { Bot, Check, Copy, Eye, Pencil, Save, Trash2 } from 'lucide-react'
import { api } from '@/api/client'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Field, FieldDescription, Input, Label, Textarea } from '@/components/ui/field'
import { localizeAgentPreset, localizePluginMetadata } from '@/i18n/builtin-metadata'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import type {
  AgentPresetDocument,
  AgentPresetSummary,
  ExtensionInventory,
  PluginCatalogEntry,
  PluginEntry,
  Profile,
} from '@/types'
import { cn } from '@/lib/utils'
import { executionTargetPath, type ExecutionTarget } from '@/domain/execution-target'
import { PluginConfigCard } from './plugin-config-card'
import { configRecord } from './plugin-config'
import { ActionDialog, SectionHeader } from './settings-ui'
import styles from './settings-layout.module.css'

interface CopyDraft {
  from: AgentPresetSummary
  id: string
  name: string
}

function presetIdError(value: string, taken: readonly AgentPresetSummary[], t: ReturnType<typeof useTranslate<'settings'>>) {
  if (!value) return t('presets.idRequired')
  if (!/^[a-z][a-z0-9-]{0,63}$/.test(value)) return t('presets.idInvalid')
  if (taken.some((preset) => preset.id === value)) return t('presets.idTaken')
  return ''
}

const emptyMetadata = (kind: string): PluginCatalogEntry => ({
  kind,
  description: '',
  requires: [],
  provides: [],
  config_schema: { type: 'object', additionalProperties: true },
})

function parseProfile(text: string): Profile {
  const value: unknown = JSON.parse(text)
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('profile')
  const plugins = (value as Record<string, unknown>).plugins
  if (!Array.isArray(plugins) || !plugins.every((plugin) => {
    if (!plugin || typeof plugin !== 'object' || Array.isArray(plugin)) return false
    const entry = plugin as Record<string, unknown>
    return (
      typeof entry.id === 'string' &&
      typeof entry.kind === 'string' &&
      typeof entry.enabled === 'boolean' &&
      Object.hasOwn(entry, 'config')
    )
  })) throw new Error('plugins')
  return value as Profile
}

function presetDisplayProfile(base: Profile | undefined, overlay: Profile): Profile {
  const plugins = new Map((base?.plugins ?? []).map(entry => [entry.id, entry]))
  for (const entry of overlay.plugins) plugins.set(entry.id, entry)
  return { plugins: [...plugins.values()] }
}

function installedExtension(entry: PluginEntry, inventory: ExtensionInventory | null) {
  if (entry.kind !== 'ternilo.extension.package') return null
  const mount = configRecord(entry.config)
  return inventory?.extensions.find((extension) => (
    extension.manifest.package_id === mount.package_id &&
    extension.manifest.version === mount.version
  )) ?? null
}

export function PresetsSettings() {
  const { presets, currentSession, currentWorkspace, catalog, refresh, updateSession, notify } = useWorkbench()
  const t = useTranslate('settings')
  const builtins = useTranslate('builtins')
  const common = useTranslate('common')
  const [copyDraft, setCopyDraft] = React.useState<CopyDraft | null>(null)
  const [document, setDocument] = React.useState<AgentPresetDocument | null>(null)
  const [editorOpen, setEditorOpen] = React.useState(false)
  const [profileDraft, setProfileDraft] = React.useState<Profile | null>(null)
  const [profileJson, setProfileJson] = React.useState('')
  const [profileJsonInvalid, setProfileJsonInvalid] = React.useState(false)
  const [inventory, setInventory] = React.useState<ExtensionInventory | null>(null)
  const [inventoryError, setInventoryError] = React.useState('')
  const [name, setName] = React.useState('')
  const [description, setDescription] = React.useState('')
  const [deleteTarget, setDeleteTarget] = React.useState<AgentPresetSummary | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const target = React.useMemo<ExecutionTarget>(() => ({
    sessionId: currentSession?.identity.session_id,
    workspaceId: currentWorkspace?.workspace_id,
  }), [currentSession?.identity.session_id, currentWorkspace?.workspace_id])

  const loadDocument = async (preset: AgentPresetSummary, edit: boolean) => {
    setBusy(true)
    setError('')
    try {
      const next = await api.request<AgentPresetDocument>(executionTargetPath(`/agent-presets/${encodeURIComponent(preset.id)}`, target))
      setDocument(next)
      setName(next.display_name)
      setDescription(next.description)
      setProfileDraft(next.profile)
      setProfileJson(JSON.stringify(next.profile, null, 2))
      setProfileJsonInvalid(false)
      setInventory(null)
      setInventoryError('')
      if (presetDisplayProfile(next.base_profile, next.profile).plugins.some((entry) => entry.kind === 'ternilo.extension.package')) {
        try {
          setInventory(
            await api.request<ExtensionInventory>(executionTargetPath('/extensions', target)),
          )
        } catch (cause) {
          setInventoryError(cause instanceof Error ? cause.message : String(cause))
        }
      }
      setEditorOpen(edit)
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    } finally {
      setBusy(false)
    }
  }

  const beginCopy = (preset: AgentPresetSummary) => {
    const display = localizeAgentPreset(preset, builtins)
    setError('')
    setCopyDraft({ from: display, id: `${preset.id}-custom`, name: `${display.display_name} · ${t('presets.custom')}` })
  }

  const copyPreset = async () => {
    if (!copyDraft) return
    const idError = presetIdError(copyDraft.id.trim(), presets.presets, t)
    if (idError) { setError(idError); return }
    setBusy(true)
    setError('')
    try {
      const id = copyDraft.id.trim()
      const displayName = copyDraft.name.trim() || id
      const created = await api.request<AgentPresetDocument>(executionTargetPath('/agent-presets', target), {
        method: 'POST',
        body: { from: copyDraft.from.id, id, display_name: displayName },
      })
      await api.request(executionTargetPath(`/agent-presets/${encodeURIComponent(id)}`, target), {
        method: 'PUT',
        body: {
          display_name: displayName,
          description: copyDraft.from.description,
          profile: created.profile,
        },
      })
      setCopyDraft(null)
      await refresh()
      notify(t('presets.copyCreated'))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const save = async () => {
    if (!document || !profileDraft || profileJsonInvalid) {
      setError(t('presets.invalidProfile'))
      return
    }
    setBusy(true)
    setError('')
    try {
      await api.request(executionTargetPath(`/agent-presets/${encodeURIComponent(document.id)}`, target), {
        method: 'PUT',
        body: { display_name: name.trim(), description: description.trim(), profile: profileDraft },
      })
      await refresh()
      setEditorOpen(false)
      setDocument(null)
      notify(t('presets.saved'))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const remove = async () => {
    if (!deleteTarget) return
    setBusy(true)
    setError('')
    try {
      await api.request(executionTargetPath(`/agent-presets/${encodeURIComponent(deleteTarget.id)}`, target), { method: 'DELETE' })
      setDeleteTarget(null)
      await refresh()
      notify(t('presets.deleted'))
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setBusy(false)
    }
  }

  const setDefault = async (preset: AgentPresetSummary) => {
    if (!presets.authorable || preset.id === presets.default_id) return
    try {
      await api.request(executionTargetPath(`/agent-presets/${encodeURIComponent(preset.id)}/default`, target), { method: 'PUT' })
      await refresh()
      notify(t('presets.defaultUpdated'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }

  const useInSession = async (preset: AgentPresetSummary) => {
    if (!currentSession || currentSession.agent_preset === preset.id) return
    try {
      await updateSession(currentSession.identity.session_id, { agent_preset: preset.id })
      notify(t('presets.sessionUpdated'))
    } catch (cause) {
      notify(cause instanceof Error ? cause.message : String(cause), 'error')
    }
  }

  const groups: Array<{ trust: 'system' | 'user'; title: string }> = [
    { trust: 'system', title: t('presets.builtInGroup') },
    { trust: 'user', title: t('presets.customGroup') },
  ]
  const localizedCatalog = (catalog?.plugins ?? []).map((plugin) => localizePluginMetadata(plugin, builtins))
  const displayedProfile = profileDraft ? presetDisplayProfile(document?.base_profile, profileDraft) : null

  const replaceProfileEntry = (nextEntry: PluginEntry) => {
    if (!profileDraft) return
    const next = {
      ...profileDraft,
      plugins: profileDraft.plugins.some(entry => entry.id === nextEntry.id)
        ? profileDraft.plugins.map(entry => entry.id === nextEntry.id ? nextEntry : entry)
        : [...profileDraft.plugins, nextEntry],
    }
    setProfileDraft(next)
    setProfileJson(JSON.stringify(next, null, 2))
    setProfileJsonInvalid(false)
    setError('')
  }

  return (
    <div className={styles.section}>
      <SectionHeader title={t('nav.presets')} description={t('presets.description')} />
      {groups.map((group) => {
        const rows = presets.presets.filter((preset) => preset.trust === group.trust)
        if (group.trust === 'system' && rows.length === 0) return null
        return (
          <section className="mb-7" key={group.trust}>
            <h3 className="mb-3 text-sm font-semibold">{group.title}</h3>
            {rows.length === 0 ? (
              <div className="rounded-xl border border-dashed p-6 text-center text-sm text-muted-foreground">{t('presets.noCustom')}</div>
            ) : (
              <div className="grid gap-3 sm:grid-cols-2">
                {rows.map((sourcePreset) => {
                  const preset = localizeAgentPreset(sourcePreset, builtins)
                  const isDefault = preset.id === presets.default_id
                  const isCurrent = currentSession?.agent_preset === preset.id
                  return (
                    <article className={cn('overflow-hidden rounded-xl border bg-card', isDefault && 'border-primary/40')} key={preset.id}>
                      <button
                        type="button"
                        className="block min-h-32 w-full p-4 text-left disabled:cursor-default"
                        disabled={isDefault || !presets.authorable}
                        aria-label={`${isDefault ? t('presets.default') : t('presets.setDefault')}: ${preset.display_name}`}
                        onClick={() => void setDefault(preset)}
                      >
                        <span className="flex flex-wrap items-center gap-2">
                          <Bot className="size-4 text-muted-foreground" />
                          <strong className="min-w-0 flex-1 truncate text-sm">{preset.display_name}</strong>
                          <span className="rounded bg-muted px-1.5 py-0.5 text-[10px] text-muted-foreground">
                            {preset.trust === 'system' ? t('presets.builtIn') : t('presets.custom')}
                          </span>
                          {isDefault ? <span className="rounded bg-primary/15 px-1.5 py-0.5 text-[10px] text-primary">{t('presets.default')}</span> : null}
                        </span>
                        <span className="mt-2 line-clamp-3 text-xs leading-relaxed text-muted-foreground">{preset.description || t('presets.noDescription')}</span>
                        <code className="mt-2 block text-[10px] text-muted-foreground">{preset.id}</code>
                      </button>
                      <div className="flex min-h-11 items-center gap-1 border-t px-2">
                        {currentSession ? (
                          <Button
                            size="xs"
                            variant="ghost"
                            disabled={isCurrent}
                            aria-label={`${t('presets.use')}: ${preset.display_name}`}
                            onClick={() => void useInSession(preset)}
                          >
                            {isCurrent ? <Check /> : null}{isCurrent ? t('presets.inUse') : t('presets.use')}
                          </Button>
                        ) : null}
                        <span className="flex-1" />
                        <Button size="icon-xs" variant="ghost" aria-label={`${t('presets.view')}: ${preset.display_name}`} onClick={() => void loadDocument(preset, false)}><Eye /></Button>
                        <Button size="icon-xs" variant="ghost" disabled={!presets.authorable} aria-label={`${t('presets.copy')}: ${preset.display_name}`} onClick={() => beginCopy(preset)}><Copy /></Button>
                        {preset.trust === 'user' ? (
                          <>
                            <Button size="icon-xs" variant="ghost" aria-label={`${t('presets.edit')}: ${preset.display_name}`} onClick={() => void loadDocument(preset, true)}><Pencil /></Button>
                            <Button size="icon-xs" variant="ghost" className="text-destructive hover:text-destructive" aria-label={`${common('delete')}: ${preset.display_name}`} onClick={() => { setError(''); setDeleteTarget(preset) }}><Trash2 /></Button>
                          </>
                        ) : null}
                      </div>
                    </article>
                  )
                })}
              </div>
            )}
          </section>
        )
      })}

      <ActionDialog
        open={copyDraft !== null}
        title={copyDraft ? `${t('presets.copyTitle')} · ${copyDraft.from.display_name}` : t('presets.copyTitle')}
        description={t('presets.copyDescription')}
        cancelLabel={common('cancel')}
        confirmLabel={t('presets.create')}
        busyLabel={t('presets.creating')}
        busy={busy}
        error={error || (copyDraft ? presetIdError(copyDraft.id.trim(), presets.presets, t) : '')}
        onOpenChange={(open) => { if (!open) { setCopyDraft(null); setError('') } }}
        onConfirm={() => void copyPreset()}
      >
        {copyDraft ? (
          <div className="grid gap-4">
            <Field><Label htmlFor="preset-copy-id">{t('presets.id')}</Label><Input id="preset-copy-id" autoFocus value={copyDraft.id} onChange={(event) => setCopyDraft({ ...copyDraft, id: event.target.value })} placeholder="my-agent" /></Field>
            <Field><Label htmlFor="preset-copy-name">{t('presets.name')}</Label><Input id="preset-copy-name" value={copyDraft.name} onChange={(event) => setCopyDraft({ ...copyDraft, name: event.target.value })} /></Field>
          </div>
        ) : null}
      </ActionDialog>

      <Dialog open={document !== null && !editorOpen} onOpenChange={(open) => { if (!open) setDocument(null) }}>
        <DialogContent className={cn(styles.settingsDialog, 'max-w-2xl')} data-settings-dialog="">
          <DialogHeader>
            <DialogTitle>{t('presets.viewTitle', { name: document ? localizeAgentPreset(document, builtins).display_name : '' })}</DialogTitle>
            <DialogDescription className="space-y-1">
              {document ? <span className="block">{localizeAgentPreset(document, builtins).description}</span> : null}
              <span className="block">{t('presets.viewDescription')}</span>
            </DialogDescription>
          </DialogHeader>
          <pre className="max-h-[55dvh] overflow-auto rounded-lg border bg-muted/25 p-4 text-xs leading-relaxed">{document ? JSON.stringify(presetDisplayProfile(document.base_profile, document.profile), null, 2) : ''}</pre>
          <DialogFooter><Button variant="outline" onClick={() => setDocument(null)}>{common('close')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={document !== null && editorOpen} onOpenChange={(open) => { if (!open && !busy) { setDocument(null); setEditorOpen(false); setError('') } }}>
        <DialogContent className={cn(styles.settingsDialog, 'max-w-2xl')} data-settings-dialog="">
          <DialogHeader>
            <DialogTitle>{t('presets.editTitle', { name: document?.display_name ?? '' })}</DialogTitle>
            <DialogDescription>{t('presets.editDescription')}</DialogDescription>
          </DialogHeader>
          <div className="grid min-w-0 max-h-[58dvh] grid-cols-[minmax(0,1fr)] gap-4 overflow-y-auto pr-1" data-preset-editor-scroll="">
            <Field><Label htmlFor="preset-edit-name">{t('presets.name')}</Label><Input id="preset-edit-name" value={name} onChange={(event) => setName(event.target.value)} /></Field>
            <Field><Label htmlFor="preset-edit-description">{t('presets.descriptionLabel')}</Label><Textarea id="preset-edit-description" value={description} onChange={(event) => setDescription(event.target.value)} /></Field>
            <section className="grid gap-3" data-preset-plugin-editor="">
              <div>
                <h3 className="text-sm font-semibold">{t('presets.plugins')}</h3>
                <p className="mt-1 text-xs leading-relaxed text-muted-foreground">{t('presets.pluginsDescription')}</p>
              </div>
              {inventoryError ? <p className="text-xs text-warning" role="status">{t('presets.extensionMetadataError')}</p> : null}
              {displayedProfile?.plugins.length ? (
                <div className="space-y-3">
                  {displayedProfile.plugins.map((entry) => {
                    const extension = installedExtension(entry, inventory)
                    const mount = configRecord(entry.config)
                    const settings = configRecord(mount.settings)
                    const metadata = extension ? {
                      kind: entry.kind,
                      description: extension.manifest.description ?? t('plugins.extensionSettingsDescription'),
                      requires: [],
                      provides: [],
                      config_schema: extension.manifest.config_schema,
                    } : localizedCatalog.find((candidate) => candidate.kind === entry.kind) ?? emptyMetadata(entry.kind)
                    const editableEntry = extension ? { ...entry, config: settings } : entry
                    return (
                      <PluginConfigCard
                        key={entry.id}
                        entry={editableEntry}
                        metadata={metadata}
                        hostToolCallLimit={catalog?.host_limits?.max_tool_calls}
                        overridden={false}
                        title={extension ? `${extension.manifest.package_id} ${extension.manifest.version}` : entry.id}
                        saveLabel={t('presets.applyPlugin')}
                        savingLabel={t('presets.applyingPlugin')}
                        onToggle={async (enabled) => replaceProfileEntry({ ...entry, enabled })}
                        onSave={async (nextEntry) => replaceProfileEntry(extension ? {
                          ...entry,
                          config: { ...mount, settings: nextEntry.config },
                        } : nextEntry)}
                      />
                    )
                  })}
                </div>
              ) : <p className="rounded-lg border border-dashed p-4 text-xs text-muted-foreground">{t('presets.noPlugins')}</p>}
            </section>
            <details className="rounded-xl border bg-muted/15 p-3">
              <summary className="cursor-pointer text-sm font-medium">{t('presets.profile')}</summary>
              <Field className="mt-3">
                <Label htmlFor="preset-effective-profile">{t('presets.effectiveProfile')}</Label>
                <FieldDescription>{t('presets.effectiveProfileDescription')}</FieldDescription>
                <Textarea
                  id="preset-effective-profile"
                  className="min-h-60 font-mono text-xs"
                  readOnly
                  value={displayedProfile ? JSON.stringify(displayedProfile, null, 2) : ''}
                />
              </Field>
              <Field className="mt-3">
                <Label htmlFor="preset-edit-profile">{t('presets.advancedProfile')}</Label>
                <FieldDescription>{t('presets.advancedProfileDescription')}</FieldDescription>
                <Textarea
                  id="preset-edit-profile"
                  className="min-h-60 font-mono text-xs"
                  value={profileJson}
                  aria-invalid={profileJsonInvalid || undefined}
                  onChange={(event) => {
                    const text = event.target.value
                    setProfileJson(text)
                    try {
                      setProfileDraft(parseProfile(text))
                      setProfileJsonInvalid(false)
                      setError('')
                    } catch {
                      setProfileJsonInvalid(true)
                    }
                  }}
                />
                {profileJsonInvalid ? <p className="text-xs text-destructive">{t('presets.invalidProfile')}</p> : null}
              </Field>
            </details>
            {error ? <p className="text-sm text-destructive" role="alert">{error}</p> : null}
          </div>
          <DialogFooter>
            <Button variant="outline" disabled={busy} onClick={() => { setDocument(null); setEditorOpen(false) }}>{common('cancel')}</Button>
            <Button disabled={busy || !name.trim() || profileJsonInvalid} onClick={() => void save()}><Save />{busy ? t('presets.saving') : t('presets.save')}</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ActionDialog
        open={deleteTarget !== null}
        title={t('presets.deleteTitle')}
        description={deleteTarget ? t('presets.deleteDescription', { name: deleteTarget.display_name }) : undefined}
        cancelLabel={common('cancel')}
        confirmLabel={common('delete')}
        busyLabel={t('presets.deleting')}
        busy={busy}
        destructive
        error={error}
        onOpenChange={(open) => { if (!open) { setDeleteTarget(null); setError('') } }}
        onConfirm={() => void remove()}
      />
    </div>
  )
}
