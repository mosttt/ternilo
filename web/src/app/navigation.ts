import * as React from 'react'

export function navigate(path: string) {
  if (`${window.location.pathname}${window.location.search}${window.location.hash}` === path) return
  window.history.pushState({}, '', path)
  window.dispatchEvent(new PopStateEvent('popstate'))
}

export function usePathname() {
  return React.useSyncExternalStore(
    listener => {
      window.addEventListener('popstate', listener)
      return () => window.removeEventListener('popstate', listener)
    },
    () => window.location.pathname,
  )
}

export function useSearch() {
  return React.useSyncExternalStore(
    listener => {
      window.addEventListener('popstate', listener)
      return () => window.removeEventListener('popstate', listener)
    },
    () => window.location.search,
  )
}
