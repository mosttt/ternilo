import { RefreshCw, ShieldAlert, ShieldCheck } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { useWorkspaceSurface } from './workspace-panel-context'

export function SandboxStatus() {
  const surface = useWorkspaceSurface()
  const t = useTranslate('settings')
  const sandbox = surface?.info?.sandbox
  const failed = sandbox?.status === 'missing' || sandbox?.status === 'unusable'
  if (!failed) return null
  const checking = Boolean(surface?.sessionId && !surface.info && !surface.error)
  const title = checking ? t('sandbox.checking') : t('sandbox.unavailable')
  const message = sandbox?.status === 'missing'
    ? sandbox.backend === 'bubblewrap' ? t('sandbox.installLinux')
      : sandbox.backend === 'windows' ? t('sandbox.installWindows') : t('sandbox.repair')
    : t('sandbox.repair')
  return <section className="my-3 min-w-0 rounded-xl border bg-muted/20 p-4 text-sm" data-sandbox-status={sandbox?.status ?? 'unknown'} role={failed ? 'status' : undefined}>
    <div className="flex items-start gap-3">
      {failed ? <ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-600" aria-hidden /> : <ShieldCheck className="mt-0.5 size-4 shrink-0 text-muted-foreground" aria-hidden />}
      <div className="min-w-0 flex-1 space-y-2">
        <strong>{title}</strong>
        {!checking && <p className="leading-relaxed text-muted-foreground">{message}</p>}
        {sandbox?.install_command && <code className="block whitespace-pre-wrap break-all rounded-md bg-muted p-2 select-all">{sandbox.install_command}</code>}
        {failed && <p className="text-muted-foreground">{t('sandbox.otherTools')}</p>}
        {(sandbox?.detail || surface?.error) && <details>
          <summary className="cursor-pointer text-muted-foreground">{t('sandbox.details')}</summary>
          <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-all text-xs">{sandbox?.detail || surface?.error}</pre>
        </details>}
        {surface?.sessionId && <Button variant="outline" size="sm" disabled={checking} onClick={() => surface.reloadInfo()}>
          <RefreshCw className={checking ? 'animate-spin' : undefined} aria-hidden />{t('sandbox.retry')}
        </Button>}
      </div>
    </div>
  </section>
}
