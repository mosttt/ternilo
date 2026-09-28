import * as React from 'react'
import { LoaderCircle, Pause, Play } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { GroupHeader } from '@/components/settings/settings-ui'
import { useTranslate } from '@/i18n/provider'
import { getExecutionStatus, setExecutionPaused, type ExecutionStatus } from './admin-api'
import css from './admin.module.css'

export function ExecutionPanel({ editable }: { editable: boolean }) {
  const t = useTranslate('admin')
  const [status, setStatus] = React.useState<ExecutionStatus | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, retry] = React.useReducer(value => value + 1, 0)
  React.useEffect(() => {
    const controller = new AbortController()
    void getExecutionStatus(controller.signal).then(value => {
      if (!controller.signal.aborted) { setStatus(value); setError('') }
    }).catch(cause => { if (!controller.signal.aborted) setError(cause instanceof Error ? cause.message : String(cause)) })
    return () => controller.abort()
  }, [revision])
  const toggle = async () => {
    if (!editable || !status || busy) return
    setBusy(true)
    setError('')
    try { setStatus(await setExecutionPaused(!status.claims_paused)) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  return <section className={css.panel} data-admin-execution="">
    <GroupHeader title={t('execution.title')} description={t('execution.description')} />
    {status ? <div className={css.execution}>
      <div><strong>{t(status.claims_paused ? 'execution.paused' : 'execution.ready')}</strong><p className={css.hint}>{t('execution.active', { runs: status.active_runs, commands: status.active_commands })}</p></div>
      {editable && <Button variant="outline" disabled={busy} onClick={() => void toggle()}>{busy ? <LoaderCircle className="animate-spin" /> : status.claims_paused ? <Play /> : <Pause />}{t(status.claims_paused ? 'execution.resume' : 'execution.pause')}</Button>}
    </div> : !error && <p className={css.hint} role="status">{t('loading')}</p>}
    {error && <div className={css.state} role="alert"><span>{error}</span><Button variant="outline" onClick={retry}>{t('retry')}</Button></div>}
  </section>
}
