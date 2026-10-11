import * as React from 'react'
import type { LocalSession } from '@/types'
import { useTranslate } from '@/i18n/provider'
import { ActionDialog } from '@/components/settings/settings-ui'

export function BlankSessionDeleteDialog({ session, onClose, onDelete }: {
  session: LocalSession
  onClose(): void
  onDelete(id: string): Promise<void>
}) {
  const t = useTranslate('workspace')
  const common = useTranslate('common')
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const submitting = React.useRef(false)
  const submit = async () => {
    if (submitting.current) return
    submitting.current = true; setBusy(true); setError('')
    try { await onDelete(session.identity.session_id); onClose() }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { submitting.current = false; setBusy(false) }
  }
  return <ActionDialog open title={t('session.deleteBlank')} description={t('session.deleteBlankDescription')}
    cancelLabel={common('cancel')} confirmLabel={t('session.deleteBlankConfirm')} busy={busy} destructive error={error}
    onOpenChange={open => { if (!open) onClose() }} onConfirm={() => void submit()} />
}
