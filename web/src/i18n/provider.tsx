import * as React from 'react'
import { en as serverSecurityEn, zh as serverSecurityZh } from './resources/server-security'
import { en as sessionArchiveEn, zh as sessionArchiveZh } from './resources/session-archive'
import { en as accountSessionsEn, zh as accountSessionsZh } from './resources/account-sessions'
import { en as adminEn, zh as adminZh } from './resources/admin'
import { en as appEn, zh as appZh } from './resources/app'
import { en as chatEn, zh as chatZh } from './resources/chat'
import { en as filesEn, zh as filesZh } from './resources/files'
import { en as builtinsEn, zh as builtinsZh } from './resources/builtins'
import { en as commonEn, zh as commonZh } from './resources/common'
import { en as conversationEn, zh as conversationZh } from './resources/conversation'
import { en as modelEn, zh as modelZh } from './resources/model'
import { en as modelServiceEn, zh as modelServiceZh } from './resources/model-service'
import { en as observabilityEn, zh as observabilityZh } from './resources/observability'
import { en as settingsEn, zh as settingsZh } from './resources/settings'
import { en as sidebarEn, zh as sidebarZh } from './resources/sidebar'
import { en as trajectoryEn, zh as trajectoryZh } from './resources/trajectory'
import { en as workspaceEn, zh as workspaceZh } from './resources/workspace'
import {
  LOCALE_STORAGE_KEY, LocaleRuntime, type BuiltInLocaleId, type LocaleNamespace,
  type Translate, storedLocale,
} from './runtime'

function createRuntime() {
  const runtime = new LocaleRuntime(storedLocale(browserStorage()))
  runtime.register('serverSecurity', { zh: serverSecurityZh, en: serverSecurityEn })
  runtime.register('sessionArchive', { zh: sessionArchiveZh, en: sessionArchiveEn })
  runtime.register('admin', { zh: adminZh, en: adminEn })
  runtime.register('accountSessions', { zh: accountSessionsZh, en: accountSessionsEn })
  runtime.register('common', { zh: commonZh, en: commonEn })
  runtime.register('app', { zh: appZh, en: appEn })
  runtime.register('sidebar', { zh: sidebarZh, en: sidebarEn })
  runtime.register('workspace', { zh: workspaceZh, en: workspaceEn })
  runtime.register('conversation', { zh: conversationZh, en: conversationEn })
  runtime.register('chat', { zh: chatZh, en: chatEn })
  runtime.register('files', { zh: filesZh, en: filesEn })
  runtime.register('builtins', { zh: builtinsZh, en: builtinsEn })
  runtime.register('trajectory', { zh: trajectoryZh, en: trajectoryEn })
  runtime.register('model', { zh: modelZh, en: modelEn })
  runtime.register('modelService', { zh: modelServiceZh, en: modelServiceEn })
  runtime.register('observability', { zh: observabilityZh, en: observabilityEn })
  runtime.register('settings', { zh: settingsZh, en: settingsEn })
  return runtime
}

function browserStorage(): Storage | undefined {
  try { return typeof localStorage === 'undefined' ? undefined : localStorage }
  catch { return undefined }
}

const LocaleContext = React.createContext<LocaleRuntime>(createRuntime())

export function LocaleProvider({ children }: { children: React.ReactNode }) {
  const [runtime] = React.useState(createRuntime)
  const snapshot = React.useSyncExternalStore(runtime.subscribe, runtime.getSnapshot, runtime.getSnapshot)
  React.useEffect(() => {
    document.documentElement.lang = snapshot.active === 'zh' ? 'zh-CN' : snapshot.active
  }, [snapshot.active])
  return <LocaleContext.Provider value={runtime}>{children}</LocaleContext.Provider>
}

export function useLocale() {
  const runtime = React.useContext(LocaleContext)
  const snapshot = React.useSyncExternalStore(runtime.subscribe, runtime.getSnapshot, runtime.getSnapshot)
  const setLocale = React.useCallback((locale: BuiltInLocaleId) => {
    browserStorage()?.setItem(LOCALE_STORAGE_KEY, locale)
    runtime.setLocale(locale)
  }, [runtime])
  return { locale: snapshot.active, setLocale }
}

export function useTranslate<N extends LocaleNamespace>(namespace: N): Translate<N> {
  const runtime = React.useContext(LocaleContext)
  React.useSyncExternalStore(runtime.subscribe, runtime.getSnapshot, runtime.getSnapshot)
  return runtime.bind(namespace)
}
