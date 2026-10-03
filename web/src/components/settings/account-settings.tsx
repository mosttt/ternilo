import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Check, Copy } from 'lucide-react'
import { api } from '@/api/client'
import { beginOidcLink, OidcFlowError } from '@/auth/oidc'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { GroupHeader, SettingRow } from './settings-ui'
import { AccountSessions } from './account-sessions'
import { AccountEmail } from './account-email'
import { AccountPassword } from './account-password'

interface OidcLink {
  native: boolean
  oidc: { issuer: string; subject: string } | null
}

export function AccountSettings() {
  const { serverIdentity: identity, serverAuthConfig: config } = useWorkbench()
  const t = useTranslate('settings')
  const appT = useTranslate('app')
  const [link, setLink] = React.useState<OidcLink | null>(null)
  const [busy, setBusy] = React.useState(false)
  const [error, setError] = React.useState('')
  const [revision, setRevision] = React.useState(0)
  const [copyState, setCopyState] = React.useState<'idle' | 'copied' | 'failed'>('idle')

  React.useEffect(() => { setCopyState('idle') }, [identity?.user.user_id])

  React.useEffect(() => {
    let cancelled = false
    setLink(null)
    setError('')
    if (identity) {
      void api.request<OidcLink>('/auth/oidc-link').then(value => {
        if (!cancelled) setLink(value)
      }).catch(cause => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause))
      })
    }
    return () => { cancelled = true }
  }, [identity?.user.user_id, revision])

  const begin = async () => {
    if (busy || !link?.native || link.oidc) return
    setBusy(true)
    setError('')
    try { await beginOidcLink() } catch (cause) {
      setError(cause instanceof OidcFlowError
        ? appT(cause.translationKey)
        : cause instanceof Error ? cause.message : String(cause))
      setBusy(false)
    }
  }

  const copyId = async () => {
    if (!identity) return
    try {
      await copyText(identity.user.user_id)
      setCopyState('copied')
    } catch { setCopyState('failed') }
  }

  if (!identity) return null
  return <section className="rounded-xl border bg-card px-4 py-4" data-account-settings="">
    <GroupHeader title={t('account.title')} description={t('account.description')} />
    <SettingRow title={t('account.current')}>
      <span className="break-all text-sm">{identity.user.username}</span>
    </SettingRow>
    <SettingRow title={t('account.email')}>
      <span className="break-all text-sm" data-account-email="">{identity.email ?? t('account.emailMissing')}</span>
    </SettingRow>
    <SettingRow title={t('account.id')}>
      <div className="max-w-80">
        <div className="flex items-center gap-2">
          <code className="min-w-0 break-all text-xs select-all" data-account-id="">{identity.user.user_id}</code>
          <Button type="button" variant="ghost" size="icon" aria-label={t('account.copyId')} title={t('account.copyId')} onClick={() => void copyId()}>{copyState === 'copied' ? <Check /> : <Copy />}</Button>
        </div>
        {copyState !== 'idle' && <p className={`mt-1 text-xs ${copyState === 'failed' ? 'text-destructive' : 'text-muted-foreground'}`} role={copyState === 'failed' ? 'alert' : 'status'}>{t(copyState === 'copied' ? 'account.idCopied' : 'account.idCopyFailed')}</p>}
      </div>
    </SettingRow>
    {config?.oidc_enabled && <SettingRow
      title={link?.oidc ? t('account.oidcLinked') : t('account.oidc')}
      description={link?.oidc ? link.oidc.issuer : t('account.oidcDescription')}
    >
      {link?.oidc ? <span className="text-sm text-muted-foreground">{t('account.linked')}</span>
        : link?.native ? <Button type="button" disabled={busy} onClick={() => void begin()}>{t(busy ? 'account.redirecting' : 'account.linkOidc')}</Button>
          : link ? <span className="text-sm text-muted-foreground">{t('account.nativeRequired')}</span>
            : error ? <Button type="button" variant="outline" onClick={() => setRevision(value => value + 1)}>{t('account.retry')}</Button>
              : <span className="text-sm text-muted-foreground" role="status">{t('account.loading')}</span>}
    </SettingRow>}
    {error && <div className="mt-2 flex flex-wrap items-center gap-2"><p className="text-sm text-destructive" role="alert">{error}</p>{!config?.oidc_enabled && <Button type="button" variant="outline" onClick={() => setRevision(value => value + 1)}>{t('account.retry')}</Button>}</div>}
    {config?.email_enabled && <AccountEmail key={`email:${identity.user.user_id}`} />}
    {link?.native && <AccountPassword key={identity.user.user_id} />}
    <AccountSessions key={identity.user.user_id} />
  </section>
}
