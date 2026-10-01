import * as React from 'react'

export const NAVIGATION_START_EVENT = 'ternilo:navigation-start'

const readLocation = () => ({ pathname: window.location.pathname, search: window.location.search })
let lastLocation = readLocation()
let closingLocation: typeof lastLocation | null = null
const listeners = new Set<() => void>()

function locationChanged() {
  const nextLocation = readLocation()
  // History already changed the URL. Keep the rendered route stable while portals close.
  closingLocation = lastLocation
  try {
    window.dispatchEvent(new Event(NAVIGATION_START_EVENT))
  } finally {
    closingLocation = null
  }
  lastLocation = nextLocation
  listeners.forEach(listener => listener())
}

function subscribe(listener: () => void) {
  if (listeners.size === 0) window.addEventListener('popstate', locationChanged)
  listeners.add(listener)
  return () => {
    listeners.delete(listener)
    if (listeners.size === 0) window.removeEventListener('popstate', locationChanged)
  }
}

function currentLocation() {
  if (closingLocation) return closingLocation
  lastLocation = readLocation()
  return lastLocation
}

export function navigate(path: string) {
  if (`${window.location.pathname}${window.location.search}${window.location.hash}` === path) return
  window.dispatchEvent(new Event(NAVIGATION_START_EVENT))
  window.history.pushState({}, '', path)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

export function usePathname() {
  return React.useSyncExternalStore(subscribe, () => currentLocation().pathname)
}

export function useSearch() {
  return React.useSyncExternalStore(subscribe, () => currentLocation().search)
}
