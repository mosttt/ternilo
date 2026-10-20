import { useRef } from 'react'
import { RefreshCw, ShieldAlert } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { useWorkspaceSurface, type SandboxReadiness } from './workspace-panel-context'

export function SandboxStatus() {
  const surface = useWorkspaceSurface()
  const t = useTranslate('settings')
  const last = useRef<{ sessionId: string | null; sandbox?: SandboxReadiness }>({ sessionId: null })
  if (surface?.info) last.current = { sessionId: surface.sessionId, sandbox: surface.info.sandbox }
  const sandbox = surface?.info ? surface.info.sandbox
    : surface?.sessionId === last.current.sessionId ? last.current.sandbox : undefined
  const failed = sandbox?.status === 'missing' || sandbox?.status === 'unusable'
  if (!failed) return null
  const checking = Boolean(surface?.sessionId && !surface.info && !surface.error)
  const title = checking ? t('sandbox.checking') : t('sandbox.unavailable')
  const message = sandbox?.status === 'missing'
    ? sandbox.backend === 'bubblewrap' ? t('sandbox.installLinux')
      : sandbox.backend === 'windows' ? t('sandbox.installWindows') : t('sandbox.repair')
    : t('sandbox.repair')
  return <section className="mx-auto my-3 w-[calc(100%-2rem)] max-w-[var(--ternilo-chat-content-width)] min-w-0 rounded-xl border bg-muted/20 p-4 text-sm" data-sandbox-status={sandbox?.status ?? 'unknown'} role="status">
    <div className="flex items-start gap-3">
      <ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-600" aria-hidden />
      <div className="min-w-0 flex-1 space-y-2">
        <strong>{title}</strong>
        {!checking && <p className="leading-relaxed text-muted-foreground">{message}</p>}
        {sandbox?.install_command && <code className="block whitespace-pre-wrap break-all rounded-md bg-muted p-2 select-all">{sandbox.install_command}</code>}
        <p className="text-muted-foreground">{t('sandbox.otherTools')}</p>
        {(sandbox?.detail || surface?.error) && <details>
          <summary className="cursor-pointer text-muted-foreground">{t('sandbox.details')}</summary>
          <pre className="mt-2 max-h-40 overflow-auto whitespace-pre-wrap break-all text-xs">{surface?.error || sandbox?.detail}</pre>
        </details>}
        {surface?.sessionId && <Button variant="outline" size="sm" disabled={checking} onClick={() => surface.reloadInfo()}>
          <RefreshCw className={checking ? 'animate-spin' : undefined} aria-hidden />{t('sandbox.retry')}
        </Button>}
      </div>
    </div>
  </section>
}
