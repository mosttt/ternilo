import type {
  PendingSubmissionEcho, SessionEvent, SessionInboxSnapshot, SessionSubmission,
} from '@/types'

function durableSubmissionId(event: SessionEvent) {
  return event.type === 'user_message' && event.source?.kind === 'submission'
    ? event.source.submission_id
    : null
}

/**
 * The server-assigned submission id is the primary identity. The client-minted
 * run id covers the interval before the admission response has returned.
 */
export function submissionEchoObserved(
  echo: PendingSubmissionEcho,
  events: readonly SessionEvent[],
  inbox: SessionInboxSnapshot | null,
) {
  if (events.some(event => (
    (echo.submission_id != null && durableSubmissionId(event) === echo.submission_id)
    || (event.type === 'user_message' && event.run_id === echo.run_id)
  ))) return true

  return inbox?.items.some(item => item.placement !== 'running' && (
    (echo.submission_id != null && item.id === echo.submission_id)
    || item.run_id === echo.run_id
  )) ?? false
}

export function visibleSubmissionEchoes(
  echoes: readonly PendingSubmissionEcho[],
  events: readonly SessionEvent[],
  inbox: SessionInboxSnapshot | null,
) {
  return echoes.filter(echo => !submissionEchoObserved(echo, events, inbox))
}

/**
 * A submission stops being pending as soon as its canonical user
 * message is visible. The local inbox may retain the consumed row until the
 * surrounding turn settles, so the event is the authoritative UI boundary.
 */
export function visibleInboxItems(
  items: readonly SessionSubmission[],
  events: readonly SessionEvent[],
) {
  const consumed = new Set(events.map(durableSubmissionId).filter((id): id is string => id != null))
  return items.filter(item => item.placement === 'running' || !consumed.has(item.id))
}
