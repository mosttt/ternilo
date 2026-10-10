import * as React from 'react'
import { en as serverSetupEn, ko as serverSetupKo, zh as serverSetupZh } from './resources/server-setup'
import { en as serviceAccountsEn, ko as serviceAccountsKo, zh as serviceAccountsZh } from './resources/service-accounts'
import { en as serverSecurityEn, ko as serverSecurityKo, zh as serverSecurityZh } from './resources/server-security'
import { en as sessionArchiveEn, ko as sessionArchiveKo, zh as sessionArchiveZh } from './resources/session-archive'
import { en as accountSessionsEn, ko as accountSessionsKo, zh as accountSessionsZh } from './resources/account-sessions'
import { en as adminEn, ko as adminKo, zh as adminZh } from './resources/admin'
import { en as appEn, ko as appKo, zh as appZh } from './resources/app'
import { en as chatEn, ko as chatKo, zh as chatZh } from './resources/chat'
import { en as filesEn, ko as filesKo, zh as filesZh } from './resources/files'
import { en as builtinsEn, ko as builtinsKo, zh as builtinsZh } from './resources/builtins'
import { en as commonEn, ko as commonKo, zh as commonZh } from './resources/common'
import { en as conversationEn, ko as conversationKo, zh as conversationZh } from './resources/conversation'
import { en as modelEn, ko as modelKo, zh as modelZh } from './resources/model'
import { en as modelServiceEn, ko as modelServiceKo, zh as modelServiceZh } from './resources/model-service'
import { en as observabilityEn, ko as observabilityKo, zh as observabilityZh } from './resources/observability'
import { en as settingsEn, ko as settingsKo, zh as settingsZh } from './resources/settings'
import { en as sidebarEn, ko as sidebarKo, zh as sidebarZh } from './resources/sidebar'
import { en as trajectoryEn, ko as trajectoryKo, zh as trajectoryZh } from './resources/trajectory'
import { en as workspaceEn, ko as workspaceKo, zh as workspaceZh } from './resources/workspace'
import {
  LOCALE_STORAGE_KEY, LocaleRuntime, type BuiltInLocaleId, type LocaleNamespace,
  type Translate, localeTag, storedLocale,
} from './runtime'

function createRuntime() {
  const runtime = new LocaleRuntime(storedLocale(browserStorage()))
  runtime.register('serverSetup', { zh: serverSetupZh, en: serverSetupEn, ko: serverSetupKo })
  runtime.register('serverSecurity', { zh: serverSecurityZh, en: serverSecurityEn, ko: serverSecurityKo })
  runtime.register('sessionArchive', { zh: sessionArchiveZh, en: sessionArchiveEn, ko: sessionArchiveKo })
  runtime.register('admin', { zh: adminZh, en: adminEn, ko: adminKo })
  runtime.register('serviceAccounts', { zh: serviceAccountsZh, en: serviceAccountsEn, ko: serviceAccountsKo })
  runtime.register('accountSessions', { zh: accountSessionsZh, en: accountSessionsEn, ko: accountSessionsKo })
  runtime.register('common', { zh: commonZh, en: commonEn, ko: commonKo })
  runtime.register('app', { zh: appZh, en: appEn, ko: appKo })
  runtime.register('sidebar', { zh: sidebarZh, en: sidebarEn, ko: sidebarKo })
  runtime.register('workspace', { zh: workspaceZh, en: workspaceEn, ko: workspaceKo })
  runtime.register('conversation', { zh: conversationZh, en: conversationEn, ko: conversationKo })
  runtime.register('chat', { zh: chatZh, en: chatEn, ko: chatKo })
  runtime.register('files', { zh: filesZh, en: filesEn, ko: filesKo })
  runtime.register('builtins', { zh: builtinsZh, en: builtinsEn, ko: builtinsKo })
  runtime.register('trajectory', { zh: trajectoryZh, en: trajectoryEn, ko: trajectoryKo })
  runtime.register('model', { zh: modelZh, en: modelEn, ko: modelKo })
  runtime.register('modelService', { zh: modelServiceZh, en: modelServiceEn, ko: modelServiceKo })
  runtime.register('observability', { zh: observabilityZh, en: observabilityEn, ko: observabilityKo })
  runtime.register('settings', { zh: settingsZh, en: settingsEn, ko: settingsKo })
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
    document.documentElement.lang = localeTag(snapshot.active)
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
