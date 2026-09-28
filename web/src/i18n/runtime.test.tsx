import { act } from 'react'
import { createRoot } from 'react-dom/client'
import { afterEach, describe, expect, it } from 'vitest'
import { LocaleProvider, useLocale, useTranslate } from './provider'
import { en as commonEn, zh as commonZh } from './resources/common'
import { LOCALE_STORAGE_KEY, LocaleRuntime } from './runtime'

const values = new Map<string, string>()
const memoryStorage = {
  get length() { return values.size },
  clear: () => values.clear(),
  getItem: (key: string) => values.get(key) ?? null,
  key: (index: number) => [...values.keys()][index] ?? null,
  removeItem: (key: string) => { values.delete(key) },
  setItem: (key: string, value: string) => { values.set(key, value) },
} satisfies Storage
Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: memoryStorage })
;(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true

describe('locale runtime', () => {
  afterEach(() => {
    localStorage.clear()
    document.documentElement.lang = ''
    document.body.innerHTML = ''
  })

  it('keeps bound translators stable and resolves the active dictionary', () => {
    const runtime = new LocaleRuntime('zh')
    runtime.register('common', { zh: commonZh, en: commonEn })
    const first = runtime.bind('common')
    expect(runtime.bind('common')).toBe(first)
    expect(first('save')).toBe('保存')
    runtime.setLocale('en')
    expect(first('save')).toBe('Save')
  })

  it('persists English and synchronizes document.lang', async () => {
    const host = document.createElement('div')
    document.body.append(host)
    const root = createRoot(host)
    function Probe() {
      const { locale, setLocale } = useLocale()
      const t = useTranslate('settings')
      return <button onClick={() => setLocale('en')}>{locale}:{t('general.language')}</button>
    }
    await act(async () => root.render(<LocaleProvider><Probe /></LocaleProvider>))
    expect(document.documentElement.lang).toBe('zh-CN')
    await act(async () => host.querySelector('button')?.click())
    expect(host.textContent).toBe('en:Language')
    expect(localStorage.getItem(LOCALE_STORAGE_KEY)).toBe('en')
    expect(document.documentElement.lang).toBe('en')
    await act(async () => root.unmount())
  })
})
