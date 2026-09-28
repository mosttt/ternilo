import * as React from 'react'

export type BusyEnterBehavior = 'queue' | 'steer'

export const BUSY_ENTER_STORAGE_KEY = 'ternilo.composer.busy-enter'
export const DEFAULT_BUSY_ENTER_BEHAVIOR: BusyEnterBehavior = 'queue'

const preferenceEvent = 'ternilo:composer-preference'

export function readBusyEnterBehavior(storage: Pick<Storage, 'getItem'>): BusyEnterBehavior {
  return storage.getItem(BUSY_ENTER_STORAGE_KEY) === 'steer' ? 'steer' : DEFAULT_BUSY_ENTER_BEHAVIOR
}

export function writeBusyEnterBehavior(
  storage: Pick<Storage, 'setItem'>,
  behavior: BusyEnterBehavior,
): void {
  storage.setItem(BUSY_ENTER_STORAGE_KEY, behavior)
  window.dispatchEvent(new CustomEvent(preferenceEvent, { detail: behavior }))
}

export function resolveComposerDelivery(
  busy: boolean,
  accelerated: boolean,
  preference: BusyEnterBehavior,
): 'queue' | 'steer' {
  if (!busy) return 'queue'
  if (!accelerated) return preference
  return preference === 'queue' ? 'steer' : 'queue'
}

export function useBusyEnterBehavior(): BusyEnterBehavior {
  const [behavior, setBehavior] = React.useState(() => readBusyEnterBehavior(window.localStorage))

  React.useEffect(() => {
    const receive = (event: Event) => {
      const next = event instanceof CustomEvent ? event.detail : readBusyEnterBehavior(window.localStorage)
      setBehavior(next === 'steer' ? 'steer' : 'queue')
    }
    window.addEventListener(preferenceEvent, receive)
    window.addEventListener('storage', receive)
    return () => {
      window.removeEventListener(preferenceEvent, receive)
      window.removeEventListener('storage', receive)
    }
  }, [])

  return behavior
}
