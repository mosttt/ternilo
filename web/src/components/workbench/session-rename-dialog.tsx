import * as React from 'react'
import type { LocalSession } from '@/types'
import { Button } from '@/components/ui/button'
import {
  Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/field'
import { useTranslate } from '@/i18n/provider'

export function SessionRenameDialog({ session, onOpenChange, onRename }: {
  session: LocalSession | null
  onOpenChange(open: boolean): void
  onRename(session: LocalSession, title: string): Promise<void>
}) {
  const t = useTranslate('workspace')
  const common = useTranslate('common')
  const [draft, setDraft] = React.useState('')
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const composing = React.useRef(false)
  const submitting = React.useRef(false)

  React.useEffect(() => {
    setDraft(session?.title ?? '')
    setError('')
  }, [session])

  const close = () => {
    if (!submitting.current) onOpenChange(false)
  }
  const submit = async () => {
    if (submitting.current) return
    const title = draft.trim()
    if (!session || !title) return
    submitting.current = true
    setSaving(true)
    setError('')
    try {
      await onRename(session, title)
      onOpenChange(false)
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      submitting.current = false
      setSaving(false)
    }
  }

  return (
    <Dialog open={session !== null} onOpenChange={open => { if (!open) close() }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>{t('rename.session.title')}</DialogTitle>
          <DialogDescription>{t('rename.session.description')}</DialogDescription>
        </DialogHeader>
        <Input
          autoFocus
          aria-label={t('field.sessionName')}
          disabled={saving}
          maxLength={120}
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
        {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" disabled={saving} onClick={close}>{common('cancel')}</Button>
          <Button type="button" disabled={!draft.trim() || saving} onClick={() => void submit()}>{saving ? common('saving') : t('rename')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
