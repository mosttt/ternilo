import { ModelTrafficAdmin, OwnModelTraffic } from './model-traffic'
import { ComputerUsage } from './computer-usage'
import { ModelDevices } from './model-devices'
import * as React from 'react'
import { ArrowLeft, ChartNoAxesCombined, KeyRound, LogOut, Settings, Sparkles } from 'lucide-react'
import { navigate, useSearch } from '@/app/navigation'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { SectionHeader, SettingsTabPanel, SettingsTabs } from '@/components/settings/settings-ui'
import { useWorkbench } from '@/state/workbench'
import { useTranslate } from '@/i18n/provider'
import { ModelProviders } from './model-providers'
import { ModelPublications } from './model-publications'
import { ModelGrants } from './model-grants'
import { AvailableModels, ModelKeys } from './model-access'
import { ModelUsage } from './model-usage'
import { AccountModels, ComputerModels, modelCenterPath } from './model-sources'
import css from './model-service.module.css'
import shell from '@/components/admin/admin.module.css'

export function ModelAdminPage() {
  const t = useTranslate('modelService')
  const { serverIdentity } = useWorkbench()
  const role = serverIdentity?.platform_role
  const manageModels = role === 'owner' || role === 'admin' || role === 'operator'
  const manageGrants = role === 'owner' || role === 'admin'
  const [tab, setTab] = React.useState('publications')
  return <section className={css.page} data-model-administration="">
    <SectionHeader title={t('adminTitle')} description={t('adminDescription')} />
    <ol className={css.setupSteps} aria-label={t('setupSteps')}>
      {[{ tab: 'providers', label: 'setupProvider' }, { tab: 'publications', label: 'setupPublication' }, { tab: 'grants', label: 'setupGrant' }].map((step, index) => <li key={step.tab}><button type="button" onClick={() => setTab(step.tab)}><span aria-hidden="true">{index + 1}</span>{t(step.label as 'setupProvider' | 'setupPublication' | 'setupGrant')}</button></li>)}
    </ol>
    <div>
      <SettingsTabs label={t('adminTitle')} tabs={[{ id: 'publications', label: t('modelsTab') }, { id: 'providers', label: t('providersTab') }, { id: 'grants', label: t('grantsTab') }, { id: 'model-usage', label: t('usageTab') }, { id: 'traffic', label: t('traffic.tab') }]} active={tab} onChange={setTab} />
      <SettingsTabPanel id="publications" active={tab}>{tab === 'publications' && <ModelPublications editable={manageModels} />}</SettingsTabPanel>
      <SettingsTabPanel id="providers" active={tab}>{tab === 'providers' && <ModelProviders editable={manageModels} />}</SettingsTabPanel>
      <SettingsTabPanel id="grants" active={tab}>{tab === 'grants' && <ModelGrants editable={manageGrants} />}</SettingsTabPanel>
      <SettingsTabPanel id="traffic" active={tab}>{tab === 'traffic' && <ModelTrafficAdmin editable={manageModels} readAccounts={role === 'owner' || role === 'admin' || role === 'auditor'} manageAccounts={manageGrants} />}</SettingsTabPanel>
      <SettingsTabPanel id="model-usage" active={tab}>{tab === 'model-usage' && <ModelUsage admin editable={manageGrants} />}</SettingsTabPanel>
    </div>
  </section>
}

function ModelAccessPage({ tab }: { tab: string }) {
  const t = useTranslate('modelService')
  const search = useSearch()
  const parameters = new URLSearchParams(search)
  const source = ['account', 'device', 'platform'].includes(parameters.get('source') ?? '') ? parameters.get('source')! : 'account'
  const access = parameters.get('access') === 'devices' ? 'devices' : 'keys'
  const usageSource = ['account', 'device', 'platform'].includes(parameters.get('usage_source') ?? '') ? parameters.get('usage_source')! : 'all'
  return <section className={css.page} data-model-access="">
    <SectionHeader title={t(tab === 'models' ? 'catalogTab' : tab === 'access' ? 'connectionsTab' : 'usageTab')} description={t('accessDescription')} />
      {tab === 'models' && <div className={css.page}>
        <SourceFilters value={source} onChange={source => navigate(modelCenterPath({ source }, search))} />
        {source === 'account' ? <AccountModels /> : source === 'device' ? <ComputerModels /> : <div className={css.page}><p className={css.hint}>{t('platformSourceDescription')}</p><AvailableModels /></div>}
      </div>}
      {tab === 'access' && <div className={css.page}>
        <OwnModelTraffic />
        <p className={css.hint}>{t('accessScopeDescription')}</p>
        <div>
          <SettingsTabs label={t('connectionsTab')} tabs={[{ id: 'keys', label: t('keysTab') }, { id: 'devices', label: t('deviceTab') }]} active={access} onChange={access => navigate(modelCenterPath({ access }, search))} />
          <SettingsTabPanel id="keys" active={access}>{access === 'keys' && <ModelKeys />}</SettingsTabPanel>
          <SettingsTabPanel id="devices" active={access}>{access === 'devices' && <ModelDevices />}</SettingsTabPanel>
        </div>
      </div>}
      {tab === 'usage' && <div className={css.page}>
        <SourceFilters value={usageSource} all onChange={usage_source => navigate(modelCenterPath({ usage_source }, search))} />
        <p className={css.hint}>{t('usageCoverage')}</p>
        {usageSource === 'device' ? <ComputerUsage /> : <ModelUsage key={usageSource} source={usageSource === 'account' ? 'user_provider' : usageSource === 'platform' ? 'platform_grant' : undefined} />}
      </div>}
  </section>
}

function ModelNavigation({ active, mobile = false }: { active: string; mobile?: boolean }) {
  const t = useTranslate('modelService')
  const search = useSearch()
  const entries = [{ id: 'models', label: t('catalogTab'), icon: Sparkles }, { id: 'access', label: t('connectionsTab'), icon: KeyRound }, { id: 'usage', label: t('usageTab'), icon: ChartNoAxesCombined }]
  return <nav className={mobile ? css.mobileNavigation : undefined} aria-label={t('accessTitle')}>
    {entries.map(({ id, label, icon: Icon }) => <a key={id} href={modelCenterPath({ tab: id }, search)} className={`${shell.navLink} ${active === id ? shell.active : ''}`} aria-current={active === id ? 'page' : undefined} onClick={event => { if (event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return; event.preventDefault(); navigate(modelCenterPath({ tab: id }, search)) }}><Icon />{label}</a>)}
  </nav>
}

function SourceFilters({ value, onChange, all = false }: { value: string; onChange(value: string): void; all?: boolean }) {
  const t = useTranslate('modelService')
  const sources = [...(all ? [{ id: 'all', label: t('allSources') }] : []), { id: 'account', label: t('accountSourceTab') }, { id: 'device', label: t('computerSourceTab') }, { id: 'platform', label: t('platformSourceTab') }]
  return <div className={css.sourceFilters} role="group" aria-label={t('sourceFilter')}>
    {sources.map(source => <Button key={source.id} variant="outline" aria-pressed={source.id === value} data-model-source-filter={source.id} onClick={() => onChange(source.id)}>{source.label}</Button>)}
  </div>
}

export function ModelAccessShell() {
  const t = useTranslate('modelService')
  const admin = useTranslate('admin')
  const sidebar = useTranslate('sidebar')
  const { serverIdentity, platform, authRequired, loading, logout } = useWorkbench()
  const search = useSearch()
  const requestedTab = new URLSearchParams(search).get('tab') ?? 'models'
  const tab = ['models', 'access', 'usage'].includes(requestedTab) ? requestedTab : 'models'
  const [readyUserId, setReadyUserId] = React.useState<string>()
  React.useEffect(() => {
    if (!loading && !authRequired && serverIdentity) setReadyUserId(serverIdentity.user.user_id)
  }, [loading, authRequired, serverIdentity?.user.user_id])
  const awaitingScope = loading && readyUserId !== serverIdentity?.user.user_id
  const contentRef = React.useRef<HTMLElement>(null)
  React.useEffect(() => { document.title = `${t('accessTitle')} · Ternilo` }, [t])
  React.useEffect(() => {
    const content = contentRef.current
    if (!content) return
    let frame = 0
    const observer = new ResizeObserver(() => {
      cancelAnimationFrame(frame)
      frame = requestAnimationFrame(() => {
        const focused = document.activeElement
        if (!(focused instanceof HTMLElement) || !content.contains(focused) || !focused.matches('input, textarea, select, [contenteditable="true"]')) return
        const bounds = content.getBoundingClientRect()
        const field = focused.getBoundingClientRect()
        if (field.top < bounds.top || field.bottom > bounds.bottom) focused.scrollIntoView({ block: 'nearest' })
      })
    })
    observer.observe(content)
    return () => { observer.disconnect(); cancelAnimationFrame(frame) }
  }, [])
  return <div className={shell.shell} data-model-access-shell="">
    <aside className={shell.sidebar}>
      <a href="/" className={shell.brand} onClick={event => { event.preventDefault(); navigate('/') }}><BrandMark /><strong>Ternilo</strong></a>
      <span className={shell.scope}>{t('accessTitle')}</span>
      <ModelNavigation active={tab} />
      <div className={shell.sidebarFooter}>
        <a href="/settings" className={shell.navLink} onClick={event => { event.preventDefault(); navigate('/settings') }}><Settings />{sidebar('userSettings')}</a>
        <a href="/" className={shell.navLink} onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft />{admin('workbench')}</a>
        <Button variant="ghost" className="justify-start" onClick={logout}><LogOut />{sidebar('logout')}</Button>
      </div>
    </aside>
    <div className={shell.main}>
      <header className={shell.header}><div className={shell.mobileNavigation}><a href="/" className={shell.back} aria-label={admin('workbench')} onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft /></a><span className="text-sm">{t('accessTitle')}</span></div><span className={shell.headerScope}>{t('accessTitle')}</span>{serverIdentity && <div className={shell.actor}>{serverIdentity.user.username}</div>}</header>
      <ModelNavigation active={tab} mobile />
      <main ref={contentRef} className={shell.content}>{authRequired || awaitingScope || (platform && !serverIdentity) ? <p role="status">{t('loading')}</p> : platform ? <ModelAccessPage key={serverIdentity?.user.user_id} tab={tab} /> : <p role="alert">{admin('denied')}</p>}</main>
    </div>
  </div>
}
