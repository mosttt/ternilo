import * as React from 'react'
import type { Workspace } from '@/types'
import { workspaceRenameIssue } from '@/domain/workspace-lifecycle'
import { Button } from '@/components/ui/button'
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'

export function WorkspaceRenameDialog({ workspace, workspaces, onOpenChange, onRename }: {
  workspace: Workspace | null
  workspaces: readonly Workspace[]
  onOpenChange(open: boolean): void
  onRename(workspace: Workspace, title: string): Promise<void>
}) {
  const t = useTranslate('workspace')
  const common = useTranslate('common')
  const [draft, setDraft] = React.useState('')
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const composing = React.useRef(false)
  const submitting = React.useRef(false)

  React.useEffect(() => {
    setDraft(workspace?.title ?? '')
    setError('')
  }, [workspace])

  const issue = workspace ? workspaceRenameIssue(draft, workspace, workspaces) : 'blank'
  const close = () => {
    if (!submitting.current) onOpenChange(false)
  }
  const submit = async () => {
    if (submitting.current || !workspace || issue !== null) return
    submitting.current = true
    setSaving(true)
    setError('')
    try {
      await onRename(workspace, draft.trim())
      onOpenChange(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      submitting.current = false
      setSaving(false)
    }
  }

  return (
    <Dialog open={workspace !== null} onOpenChange={open => { if (!open) close() }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('rename.workspace.title')}</DialogTitle>
          <DialogDescription>{t('rename.workspace.description')}</DialogDescription>
        </DialogHeader>
        <Input
          autoFocus
          aria-label={t('field.workspaceName')}
          disabled={saving}
          value={draft}
          onFocus={event => event.currentTarget.select()}
          onChange={event => { setDraft(event.target.value); setError('') }}
          onCompositionStart={() => { composing.current = true }}
          onCompositionEnd={() => { composing.current = false }}
          onKeyDown={event => {
            if (event.key !== 'Enter' || composing.current) return
            event.preventDefault()
            void submit()
          }}
        />
        {issue === 'duplicate' && <p role="alert" className="text-sm text-destructive">{t('conflict.named', { name: draft.trim() })}</p>}
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={saving} onClick={close}>{common('cancel')}</Button>
          <Button type="button" disabled={issue !== null || saving} onClick={() => void submit()}>{saving ? common('saving') : t('rename')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function WorkspaceUnregisterDialog({ workspace, onOpenChange, onUnregister }: {
  workspace: Workspace | null
  onOpenChange(open: boolean): void
  onUnregister(workspace: Workspace): Promise<void>
}) {
  const t = useTranslate('workspace')
  const common = useTranslate('common')
  const [removing, setRemoving] = React.useState(false)
  const [error, setError] = React.useState('')

  React.useEffect(() => { setError('') }, [workspace])

  const close = () => {
    if (!removing) onOpenChange(false)
  }
  const submit = async () => {
    if (!workspace || removing) return
    setRemoving(true)
    setError('')
    try {
      await onUnregister(workspace)
      onOpenChange(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      setRemoving(false)
    }
  }

  return (
    <Dialog open={workspace !== null} onOpenChange={open => { if (!open) close() }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('delete.workspace')}</DialogTitle>
          <DialogDescription>
            {t('delete.desc', { name: workspace?.title ?? '' })}
          </DialogDescription>
        </DialogHeader>
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={removing} onClick={close}>{common('cancel')}</Button>
          <Button type="button" variant="destructive" disabled={removing} onClick={() => void submit()}>{removing ? t('delete.pending') : t('delete.workspace')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
