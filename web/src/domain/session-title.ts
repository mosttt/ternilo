import type { SessionEvent } from '@/types'

/**
 * Keep the transient title visible until the generated title reaches Session
 * metadata. A failed generation settles immediately back to the persisted
 * title, while a successful finish remains pending until that title changes.
 */
export function sessionTitleGenerationPending(events: readonly SessionEvent[]) {
  let pending = false
  for (const event of events) {
    if (event.type === 'session_title_generation_started') pending = true
    if (event.type === 'session_title_generation_finished') pending = event.generated === true
  }
  return pending
}
