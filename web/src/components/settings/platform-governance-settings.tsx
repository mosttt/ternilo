import * as React from 'react'
import { LoaderCircle, RefreshCw, Save } from 'lucide-react'
import { ApiError } from '@/api/client'
import { Button } from '@/components/ui/button'
import { Field, FieldDescription, Input, Label } from '@/components/ui/field'
import { useLocale, useTranslate } from '@/i18n/provider'
import type { AuditEntry, TenantQuota } from '@/types'
import { getTenantQuota, listTenantAudit, updateTenantQuota } from './platform-admin-api'
import { GroupHeader } from './settings-ui'
import styles from './platform-settings.module.css'

const AUDIT_LIMIT = 200

type QuotaDraft = Record<keyof TenantQuota, string>

const quotaFields: Array<keyof TenantQuota> = [
  'max_nodes',
  'max_concurrent_runs',
  'monthly_model_tokens',
  'max_secrets',
]

function quotaDraft(value: TenantQuota): QuotaDraft {
  return {
    max_nodes: String(value.max_nodes),
    max_concurrent_runs: String(value.max_concurrent_runs),
    monthly_model_tokens: String(value.monthly_model_tokens),
    max_secrets: String(value.max_secrets),
  }
}

export function parseQuotaDraft(value: QuotaDraft): TenantQuota | null {
  const parsed = Object.fromEntries(quotaFields.map((field) => [field, Number(value[field])])) as unknown as TenantQuota
  return quotaFields.every((field) => Number.isSafeInteger(parsed[field]) && parsed[field] > 0)
    ? parsed
    : null
}

function permissionError(cause: unknown, fallback: string, permission: string) {
  return cause instanceof ApiError && cause.status === 403 ? permission : fallback
}

export function PlatformQuotaSettings({
  tenantId,
  editable,
}: {
  tenantId: string
  editable: boolean
}) {
  const t = useTranslate('settings')
  const [quota, setQuota] = React.useState<TenantQuota | null>(null)
  const [draft, setDraft] = React.useState<QuotaDraft | null>(null)
  const [loading, setLoading] = React.useState(true)
  const [saving, setSaving] = React.useState(false)
  const [error, setError] = React.useState('')
  const [message, setMessage] = React.useState('')

  const load = React.useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      const next = await getTenantQuota(tenantId)
      setQuota(next)
      setDraft(quotaDraft(next))
    } catch (cause) {
      setError(permissionError(cause, t('quota.loadError'), t('platform.permission')))
    } finally {
      setLoading(false)
    }
  }, [t, tenantId])

  React.useEffect(() => { void load() }, [load])

  const parsed = draft ? parseQuotaDraft(draft) : null
  const dirty = Boolean(parsed && quota && quotaFields.some((field) => parsed[field] !== quota[field]))
  const fieldCopy: Record<keyof TenantQuota, { label: string; description: string }> = {
    max_nodes: { label: t('quota.max_nodes'), description: t('quota.max_nodesDescription') },
    max_concurrent_runs: { label: t('quota.max_concurrent_runs'), description: t('quota.max_concurrent_runsDescription') },
    monthly_model_tokens: { label: t('quota.monthly_model_tokens'), description: t('quota.monthly_model_tokensDescription') },
    max_secrets: { label: t('quota.max_secrets'), description: t('quota.max_secretsDescription') },
  }
  const save = async () => {
    if (!parsed || !editable || saving) return
    setSaving(true)
    setError('')
    setMessage('')
    try {
      await updateTenantQuota(tenantId, parsed)
      setQuota(parsed)
      setDraft(quotaDraft(parsed))
      setMessage(t('quota.saved'))
    } catch (cause) {
      setError(permissionError(cause, t('quota.saveError'), t('platform.permission')))
    } finally {
      setSaving(false)
    }
  }

  const listState = loading && !quota ? 'loading' : error && !quota ? 'error' : quota ? 'ready' : 'empty'
  return (
    <div data-platform-quota="" data-platform-list-state={listState}>
      <GroupHeader title={t('quota.title')} description={t(editable ? 'quota.descriptionOwner' : 'quota.descriptionAdmin')} />
      {loading && quota ? <p className="mb-3 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p> : null}
      {error && quota ? <p className="mb-3 text-sm text-destructive" role="alert">{error}</p> : null}
      {message ? <p className="mb-3 text-sm text-success" role="status">{message}</p> : null}

      {loading && !quota ? (
        <div className={styles.statePanel} role="status"><div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div></div>
      ) : error && !quota ? (
        <div className={styles.statePanel} role="alert"><div><span>{error}</span><Button type="button" variant="outline" onClick={() => void load()}>{t('platform.retry')}</Button></div></div>
      ) : draft ? (
        <form className={styles.quotaForm} onSubmit={(event) => { event.preventDefault(); void save() }}>
          <div className={styles.quotaGrid}>
            {quotaFields.map((field) => (
              <Field key={field}>
                <Label htmlFor={`platform-quota-${field}`}>{fieldCopy[field].label}</Label>
                <Input
                  id={`platform-quota-${field}`}
                  type="number"
                  min={1}
                  step={1}
                  inputMode="numeric"
                  readOnly={!editable}
                  value={draft[field]}
                  onChange={(event) => setDraft((current) => current ? { ...current, [field]: event.target.value } : current)}
                />
                <FieldDescription>{fieldCopy[field].description}</FieldDescription>
              </Field>
            ))}
          </div>
          {editable ? (
            <div className={styles.quotaActions}>
              {!parsed ? <span className="text-sm text-destructive" role="alert">{t('quota.invalid')}</span> : <span />}
              <Button type="submit" disabled={!parsed || !dirty || saving}>
                {saving ? <LoaderCircle className={styles.spinner} /> : <Save />}
                {t(saving ? 'quota.saving' : 'quota.save')}
              </Button>
            </div>
          ) : null}
        </form>
      ) : null}
    </div>
  )
}

export function PlatformAuditSettings({ tenantId }: { tenantId: string }) {
  const t = useTranslate('settings')
  const { locale } = useLocale()
  const [entries, setEntries] = React.useState<AuditEntry[]>([])
  const [loading, setLoading] = React.useState(true)
  const [error, setError] = React.useState('')
  const load = React.useCallback(async () => {
    setLoading(true)
    setError('')
    try {
      setEntries(await listTenantAudit(tenantId, AUDIT_LIMIT))
    } catch (cause) {
      setError(permissionError(cause, t('audit.loadError'), t('platform.permission')))
    } finally {
      setLoading(false)
    }
  }, [t, tenantId])

  React.useEffect(() => { void load() }, [load])

  const formatTime = React.useCallback((value: number) => new Intl.DateTimeFormat(
    locale === 'zh' ? 'zh-CN' : 'en',
    { dateStyle: 'medium', timeStyle: 'medium' },
  ).format(new Date(value)), [locale])
  const listState = loading && !entries.length
    ? 'loading'
    : error && !entries.length
      ? 'error'
      : entries.length
        ? 'ready'
        : 'empty'

  return (
    <div data-platform-audit="" data-platform-list-state={listState}>
      <div className={styles.auditHeader}>
        <GroupHeader title={t('audit.title')} description={t('audit.description', { limit: AUDIT_LIMIT })} />
        <Button type="button" variant="ghost" size="sm" disabled={loading} onClick={() => void load()}>
          <RefreshCw className={loading ? styles.spinner : ''} />{t('platform.refresh')}
        </Button>
      </div>
      {loading && entries.length ? <p className="mb-3 text-sm text-muted-foreground" role="status">{t('platform.loading')}</p> : null}
      {error && entries.length ? <p className="mb-3 text-sm text-destructive" role="alert">{error}</p> : null}

      {loading && !entries.length ? (
        <div className={styles.statePanel} role="status"><div><LoaderCircle className={styles.spinner} />{t('platform.loading')}</div></div>
      ) : error && !entries.length ? (
        <div className={styles.statePanel} role="alert"><div><span>{error}</span><Button type="button" variant="outline" onClick={() => void load()}>{t('platform.retry')}</Button></div></div>
      ) : entries.length ? (
        <div className={styles.auditList} aria-label={t('audit.list')}>
          {[...entries].reverse().map((entry) => (
            <article className={styles.auditRow} data-platform-audit-entry={entry.audit_id} key={entry.audit_id}>
              <div className={styles.auditPrimary}>
                <strong>{entry.action}</strong>
                <span className={styles.state} data-outcome={entry.outcome}>{entry.outcome}</span>
                <time dateTime={new Date(entry.occurred_at_ms).toISOString()}>{formatTime(entry.occurred_at_ms)}</time>
              </div>
              <dl className={styles.auditFacts}>
                <div><dt>{t('audit.actor')}</dt><dd>{entry.actor_user_id ?? entry.actor_kind}</dd></div>
                <div><dt>{t('audit.resource')}</dt><dd>{entry.resource_type} · {entry.resource_id}</dd></div>
                <div><dt>{t('audit.id')}</dt><dd><code>{entry.audit_id}</code></dd></div>
                <div><dt>{t('audit.hash')}</dt><dd><code>{entry.entry_hash_hex}</code></dd></div>
              </dl>
              <details className={styles.auditDetails}>
                <summary>{t('audit.metadata')}</summary>
                <pre>{JSON.stringify(entry.metadata, null, 2)}</pre>
              </details>
            </article>
          ))}
        </div>
      ) : <div className={styles.statePanel}>{t('audit.empty')}</div>}
    </div>
  )
}
