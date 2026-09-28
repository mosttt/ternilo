export function installDesktopContextMenu(): () => void {
  if (!window.__TAURI__?.core?.invoke) return () => {}
  const preventDefaultMenu = (event: MouseEvent) => event.preventDefault()
  document.addEventListener('contextmenu', preventDefaultMenu)
  return () => document.removeEventListener('contextmenu', preventDefaultMenu)
}
