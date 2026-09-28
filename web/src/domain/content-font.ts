export const contentFontSizes = [14, 16, 18] as const

export type ContentFontSize = (typeof contentFontSizes)[number]

export const contentFontStorageKey = 'ternilo.content-font-size'

export function readContentFontSize(storage: Pick<Storage, 'getItem'>): ContentFontSize {
  const value = Number(storage.getItem(contentFontStorageKey))
  return contentFontSizes.includes(value as ContentFontSize) ? value as ContentFontSize : 14
}

export function applyContentFontSize(size: ContentFontSize): void {
  document.documentElement.style.setProperty('--ternilo-content-font-size', `${size}px`)
}

export function writeContentFontSize(storage: Pick<Storage, 'setItem'>, size: ContentFontSize): void {
  storage.setItem(contentFontStorageKey, String(size))
  applyContentFontSize(size)
}
