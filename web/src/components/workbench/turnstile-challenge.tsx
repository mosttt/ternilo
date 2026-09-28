import * as React from 'react'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'

interface TurnstileApi {
  render(container: HTMLElement, options: {
    sitekey: string
    action: string
    theme: 'light' | 'dark'
    size: 'flexible' | 'compact'
    callback(token: string): void
    'expired-callback'(): void
    'error-callback'(): void
  }): string
  remove(widget: string): void
}

declare global { interface Window { turnstile?: TurnstileApi } }

let loading: Promise<TurnstileApi> | undefined

function loadTurnstile(): Promise<TurnstileApi> {
  if (window.turnstile) return Promise.resolve(window.turnstile)
  if (loading) return loading
  loading = new Promise<TurnstileApi>((resolve, reject) => {
    const script = document.createElement('script')
    script.src = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit'
    script.async = true
    const fail = () => {
      clearTimeout(timeout)
      script.remove()
      loading = undefined
      reject(new Error('Turnstile could not load'))
    }
    const timeout = window.setTimeout(fail, 15000)
    script.onerror = fail
    script.onload = () => {
      clearTimeout(timeout)
      if (window.turnstile) resolve(window.turnstile)
      else fail()
    }
    document.head.append(script)
  })
  return loading
}

export function TurnstileChallenge({ siteKey, action, attempt, onToken }: {
  siteKey: string
  action: 'login' | 'register' | 'invitation'
  attempt: number
  onToken(token: string): void
}) {
  const t = useTranslate('serverSecurity')
  const container = React.useRef<HTMLDivElement>(null)
  const callback = React.useRef(onToken)
  callback.current = onToken
  const [retry, setRetry] = React.useState(0)
  const [status, setStatus] = React.useState<'loading' | 'ready' | 'error'>('loading')

  React.useEffect(() => {
    let disposed = false
    let widget: string | undefined
    let provider: TurnstileApi | undefined
    let observer: ResizeObserver | undefined
    let themeObserver: MutationObserver | undefined
    callback.current('')
    setStatus('loading')
    const failed = () => {
      if (!disposed) { callback.current(''); setStatus('error') }
    }
    void loadTurnstile().then(api => {
      if (disposed || !container.current) return
      provider = api
      let compact: boolean | undefined
      let theme: 'light' | 'dark' | undefined
      const render = () => {
        if (disposed || !container.current) return
        const nextCompact = container.current.clientWidth < 300
        const nextTheme = document.documentElement.classList.contains('dark') ? 'dark' : 'light'
        if (widget && compact === nextCompact && theme === nextTheme) return
        if (widget) api.remove(widget)
        callback.current('')
        setStatus('loading')
        compact = nextCompact
        theme = nextTheme
        widget = api.render(container.current, {
          sitekey: siteKey, action, theme, size: compact ? 'compact' : 'flexible',
          callback: token => { if (!disposed) { callback.current(token); setStatus('ready') } },
          'expired-callback': failed,
          'error-callback': failed,
        })
      }
      render()
      observer = new ResizeObserver(render)
      observer.observe(container.current)
      themeObserver = new MutationObserver(render)
      themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
    }).catch(failed)
    return () => { disposed = true; observer?.disconnect(); themeObserver?.disconnect(); if (widget) provider?.remove(widget) }
  }, [siteKey, action, attempt, retry])

  return <div className="grid min-w-0 gap-2" aria-label={t('challenge')}>
    <div ref={container} />
    {status === 'loading' && <p className="text-xs text-muted-foreground" role="status">{t('challengeLoading')}</p>}
    {status === 'error' && <>
      <p className="text-xs text-destructive" role="alert">{t('challengeError')}</p>
      <Button type="button" variant="outline" className="w-fit" onClick={() => setRetry(value => value + 1)}>{t('retry')}</Button>
    </>}
  </div>
}
