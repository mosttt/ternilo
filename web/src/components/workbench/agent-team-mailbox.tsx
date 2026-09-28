import * as React from 'react'
import { CheckCheck, LoaderCircle, Send } from 'lucide-react'
import type { Translate } from '@/i18n/runtime'
import type { AgentTeamSnapshot } from '@/types'
import { Button } from '@/components/ui/button'
import { Field, Label, Select, Textarea } from '@/components/ui/field'
import { useLocale } from '@/i18n/provider'
import css from './agent-team-panel.module.css'

export function AgentTeamMailbox({
  snapshot,
  pending,
  readOnly = false,
  initialRecipient,
  onSend,
  onMarkRead,
  t,
}: {
  snapshot: AgentTeamSnapshot
  pending: string | null
  readOnly?: boolean
  initialRecipient: string | null
  onSend(to: string, content: string): Promise<boolean>
  onMarkRead(messageId: string): Promise<boolean>
  t: Translate<'observability'>
}) {
  const { locale } = useLocale()
  const recipients = snapshot.members.filter(member => member.id !== snapshot.current_member_id)
  const [recipient, setRecipient] = React.useState(() => initialRecipient ?? recipients[0]?.id ?? '')
  const [content, setContent] = React.useState('')
  const names = React.useMemo(() => new Map(snapshot.members.map(member => [member.id, member.label])), [snapshot.members])
  const formatter = React.useMemo(() => new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', {
    month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
  }), [locale])

  React.useEffect(() => {
    setRecipient(current => {
      if (initialRecipient && recipients.some(member => member.id === initialRecipient)) return initialRecipient
      return recipients.some(member => member.id === current) ? current : recipients[0]?.id ?? ''
    })
  }, [initialRecipient, snapshot.current_member_id, snapshot.members])

  const submit = async (event: React.FormEvent) => {
    event.preventDefault()
    const message = content.trim()
    if (!recipient || !message) return
    const sent = await onSend(recipient, message)
    if (sent) setContent('')
  }

  return <div className={css.mailbox} data-agent-team-mailbox="">
    <div className={css.viewIntro}>
      <div><strong>{t('team.mailbox.title')}</strong><p>{t('team.mailbox.description')}</p></div>
    </div>
    {!readOnly && (recipients.length > 0 ? <form className={css.messageForm} onSubmit={event => void submit(event)}>
      <Field>
        <Label htmlFor="team-message-recipient">{t('team.mailbox.to')}</Label>
        <Select id="team-message-recipient" value={recipient} disabled={pending !== null} onValueChange={nextValue => setRecipient(nextValue)}>
          {recipients.map(member => <option key={member.id} value={member.id}>{member.label}</option>)}
        </Select>
      </Field>
      <Field className={css.messageField}>
        <Label htmlFor="team-message-content">{t('team.mailbox.message')}</Label>
        <Textarea id="team-message-content" rows={2} value={content} disabled={pending !== null} placeholder={t('team.mailbox.placeholder')} onChange={event => setContent(event.target.value)} />
      </Field>
      <Button type="submit" size="sm" disabled={pending !== null || !recipient || !content.trim()}>
        {pending === 'send' ? <LoaderCircle className={css.spin} aria-hidden="true" /> : <Send aria-hidden="true" />}
        {pending === 'send' ? t('team.sending') : t('team.send')}
      </Button>
    </form> : <div className={css.emptyState}><strong>{t('team.mailbox.noRecipient')}</strong><p>{t('team.mailbox.noRecipientHint')}</p></div>)}
    {!snapshot.messages.length && <div className={css.emptyState}><strong>{t('team.mailbox.empty')}</strong><p>{t('team.mailbox.emptyHint')}</p></div>}
    <ol className={css.messageList} aria-label={t('team.mailbox.list')}>
      {[...snapshot.messages].sort((left, right) => right.created_at_ms - left.created_at_ms).map(message => {
        const incoming = message.to === snapshot.current_member_id
        const unread = !message.read_at_ms
        const canMarkRead = incoming && unread
        const counterpart = names.get(incoming ? message.from : message.to) ?? t('team.member.unknown')
        return <li className={css.messageCard} data-direction={incoming ? 'incoming' : 'outgoing'} data-unread={unread || undefined} key={message.id}>
          <header>
            <strong>{incoming ? t('team.mailbox.from', { name: counterpart }) : t('team.mailbox.sentTo', { name: counterpart })}</strong>
            <time dateTime={new Date(message.created_at_ms).toISOString()}>{formatter.format(new Date(message.created_at_ms))}</time>
          </header>
          <p>{message.content}</p>
          <footer>
            <span>{unread ? t('team.mailbox.unread') : t('team.mailbox.read')}</span>
            {!readOnly && canMarkRead && <Button type="button" size="xs" variant="ghost" disabled={pending !== null} onClick={() => void onMarkRead(message.id)}>
              {pending === 'read' ? <LoaderCircle className={css.spin} aria-hidden="true" /> : <CheckCheck aria-hidden="true" />}
              {t('team.mailbox.markRead')}
            </Button>}
          </footer>
        </li>
      })}
    </ol>
  </div>
}
