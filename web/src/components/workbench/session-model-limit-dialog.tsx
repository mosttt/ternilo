import * as React from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/field'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { useTranslate } from '@/i18n/provider'

export function SessionModelLimitDialog({ limit, onSave, onClose }: {
  limit: number | null | undefined
  onSave(value: number): Promise<void>
  onClose(): void
}) {
  const t = useTranslate('conversation')
  const common = useTranslate('common')
  const [draft, setDraft] = React.useState(String(limit ?? ''))
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const submitting = React.useRef(false)
  const value = Number(draft)
  const valid = Number.isSafeInteger(value) && value > 0
  const close = () => { if (!submitting.current) onClose() }
  const submit = async () => {
    if (submitting.current || !valid) return
    submitting.current = true
    setSaving(true)
    setError('')
    try {
      await onSave(value)
      onClose()
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause))
    } finally {
      submitting.current = false
      setSaving(false)
    }
  }
  return <Dialog open onOpenChange={open => { if (!open) close() }}>
    <DialogContent className="max-w-md overflow-y-auto" showClose={!saving}>
      <DialogHeader>
        <DialogTitle>{t('session.modelLimit')}</DialogTitle>
        <DialogDescription>{t('session.modelLimitDescription')}</DialogDescription>
      </DialogHeader>
      <label className="grid gap-2 text-sm">
        {t('session.modelLimitTokens')}
        <Input autoFocus type="number" inputMode="numeric" min={1} step={1} disabled={saving} value={draft}
          onChange={event => { setDraft(event.target.value); setError('') }}
          onKeyDown={event => { if (event.key === 'Enter') { event.preventDefault(); void submit() } }} />
      </label>
      {draft && !valid && <p role="alert" className="text-sm text-destructive">{t('session.modelLimitInvalid')}</p>}
      {error && <p role="alert" className="text-sm text-destructive">{error}</p>}
      <p className="text-sm text-muted-foreground">{t('session.modelLimitReservation')}</p>
      <DialogFooter>
        <Button type="button" variant="outline" disabled={saving} onClick={close}>{common('cancel')}</Button>
        <Button type="button" disabled={saving || !valid} onClick={() => void submit()}>{common(saving ? 'saving' : 'save')}</Button>
      </DialogFooter>
    </DialogContent>
  </Dialog>
}
