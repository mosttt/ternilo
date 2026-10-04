
export const LOCALE_IDS = ['zh', 'en'] as const
export type BuiltInLocaleId = typeof LOCALE_IDS[number]
export type LocaleDict = Record<string, string>

/** Resource modules merge their key unions into this map. */
export interface LocaleNamespaceMap {}

export type LocaleNamespace = Extract<keyof LocaleNamespaceMap, string>
export type LocaleKey<N extends LocaleNamespace> = Extract<LocaleNamespaceMap[N], string>
export type Translate<N extends LocaleNamespace> = (
  key: LocaleKey<N>,
  params?: Record<string, unknown>,
) => string

export interface LocaleSnapshot {
  active: BuiltInLocaleId
  revision: number
}

export const LOCALE_STORAGE_KEY = 'ternilo.locale'

export class LocaleRuntime {
  private readonly dicts = new Map<string, Map<BuiltInLocaleId, LocaleDict>>()
  private readonly bound = new Map<string, (key: string, params?: Record<string, unknown>) => string>()
  private readonly listeners = new Set<() => void>()
  private snapshot: LocaleSnapshot

  constructor(initial: BuiltInLocaleId = 'zh') {
    this.snapshot = Object.freeze({ active: initial, revision: 0 })
  }

  getSnapshot = (): LocaleSnapshot => this.snapshot

  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  setLocale(id: BuiltInLocaleId) {
    if (id === this.snapshot.active) return
    this.publish(id)
  }

  register<N extends LocaleNamespace>(
    namespace: N,
    dictionaries: Record<BuiltInLocaleId, Record<LocaleKey<N>, string>>,
  ) {
    if (this.dicts.has(namespace)) throw new Error(`locale namespace "${namespace}" is already registered`)
    this.dicts.set(namespace, new Map([
      ['zh', dictionaries.zh],
      ['en', dictionaries.en],
    ]))
    this.publish(this.snapshot.active)
  }

  bind<N extends LocaleNamespace>(namespace: N): Translate<N> {
    let translate = this.bound.get(namespace)
    if (!translate) {
      translate = (key, params) => this.translate(namespace, key, params)
      this.bound.set(namespace, translate)
    }
    return translate as Translate<N>
  }

  private translate(namespace: string, key: string, params?: Record<string, unknown>) {
    const locales = this.dicts.get(namespace)
    const template = locales?.get(this.snapshot.active)?.[key]
      ?? locales?.get('en')?.[key]
      ?? this.dicts.get('common')?.get(this.snapshot.active)?.[key]
      ?? this.dicts.get('common')?.get('en')?.[key]
      ?? key
    if (!params) return template
    return template.replace(/\{(\w+)\}/g, (match, name: string) =>
      Object.hasOwn(params, name) ? String(params[name]) : match)
  }

  private publish(active: BuiltInLocaleId) {
    this.snapshot = Object.freeze({ active, revision: this.snapshot.revision + 1 })
    for (const listener of [...this.listeners]) listener()
  }
}

export function storedLocale(
  storage: Pick<Storage, 'getItem'> | undefined,
  languages: readonly string[] = typeof navigator === 'undefined' ? []
    : navigator.languages.length ? navigator.languages : [navigator.language],
): BuiltInLocaleId {
  const saved = storage?.getItem(LOCALE_STORAGE_KEY)
  if (saved === 'zh' || saved === 'en') return saved
  for (const language of languages) {
    const base = language.toLowerCase().split('-')[0]
    if (base === 'zh' || base === 'en') return base
  }
  return 'en'
}
