import { copyText } from '@/lib/clipboard'
import * as React from 'react'
import { Bot, Check, CircleHelp, Clock3, Copy, UserRound } from 'lucide-react'
import { Popover } from 'radix-ui'
import type { InputAuthor } from '@/types'
import { useTranslate } from '@/i18n/provider'
import { useInputViewer } from '@/state/input-viewer'
import css from './input-identity.module.css'

export function InputIdentity({ author, compact = false }: { author?: InputAuthor | null; compact?: boolean }) {
  const viewer = useInputViewer()
  const t = useTranslate('chat')
  const account = author?.kind === 'account' ? author : null
  const own = account ? account.user_id === viewer.user?.user_id : author?.kind === 'local' && viewer.local
  const label = own ? t('identity.you') : account ? account.username
    : author?.kind === 'local' ? t('identity.localUser')
      : author?.kind === 'automation' ? t(author.source === 'schedule' ? 'identity.schedule' : 'identity.subagent')
        : t('identity.unrecorded')
  const Icon = author?.kind === 'automation' ? author.source === 'schedule' ? Clock3 : Bot
    : author ? UserRound : CircleHelp
  const identityKey = JSON.stringify([author, viewer.local, viewer.user?.user_id])
  const currentIdentity = React.useRef(identityKey)
  currentIdentity.current = identityKey
  const [open, setOpen] = React.useState(false)
  const [copyState, setCopyState] = React.useState<'idle' | 'copied' | 'failed'>('idle')
  const titleId = React.useId()
  React.useEffect(() => { setOpen(false); setCopyState('idle') }, [identityKey])

  const copy = async () => {
    if (!account) return
    const copiedIdentity = identityKey
    try {
      await copyText(account.user_id)
      if (currentIdentity.current === copiedIdentity) setCopyState('copied')
    } catch {
      if (currentIdentity.current === copiedIdentity) setCopyState('failed')
    }
  }

  return <Popover.Root open={open} onOpenChange={value => { setOpen(value); setCopyState('idle') }}>
    <Popover.Trigger asChild>
      <button type="button" className={`${css.trigger} ${compact ? css.compact : ''}`} data-input-identity="" data-input-author={author?.kind ?? 'unrecorded'} aria-label={t('identity.open', { name: label })}>
        <span className={css.avatar} aria-hidden="true">{account ? account.username.slice(0, 2).toUpperCase() : <Icon />}</span>
        <span className={css.name} data-input-identity-label="">{label}</span>
      </button>
    </Popover.Trigger>
    <Popover.Portal><Popover.Content className={css.details} side="bottom" align="end" sideOffset={6} collisionPadding={12} aria-labelledby={titleId}>
      <h3 id={titleId}>{t('identity.details')}</h3>
      {account ? <>
        <dl>
          <dt>{t('identity.username')}</dt><dd>{account.username}</dd>
          <dt>{t('identity.id')}</dt><dd className={css.idRow}>
            <code className={css.id} data-input-account-id="">{account.user_id}</code>
            <button type="button" className={css.copy} onClick={() => void copy()} aria-label={t('identity.copyId')}>{copyState === 'copied' ? <Check /> : <Copy />}</button>
          </dd>
        </dl>
        {copyState !== 'idle' && <p className={copyState === 'failed' ? css.error : css.notice} role={copyState === 'failed' ? 'alert' : 'status'}>{t(copyState === 'copied' ? 'identity.copied' : 'identity.copyFailed')}</p>}
      </> : <p>{t(author?.kind === 'local' ? 'identity.localDescription'
        : author?.kind === 'automation' ? author.source === 'schedule' ? 'identity.scheduleDescription' : 'identity.subagentDescription'
          : 'identity.unrecordedDescription')}</p>}
    </Popover.Content></Popover.Portal>
  </Popover.Root>
}
