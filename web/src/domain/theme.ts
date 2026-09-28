export type ThemePreference = 'light' | 'dark' | 'system'

/** Resolves the user's preference for the shared interface and browser chrome. */
export function applyThemePreference(
  preference: ThemePreference,
  prefersDark = matchMedia('(prefers-color-scheme: dark)').matches,
): void {
  const dark = preference === 'dark' || (preference === 'system' && prefersDark)
  document.documentElement.classList.toggle('dark', dark)
  document.documentElement.dataset.theme = preference
  document.documentElement.style.colorScheme = dark ? 'dark' : 'light'
  document.querySelector<HTMLMetaElement>('meta[name="theme-color"]')
    ?.setAttribute('content', dark ? '#191a1f' : '#ffffff')
}
