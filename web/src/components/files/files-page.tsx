import * as React from 'react'
import { ArrowLeft, ArrowUpRight, CloudOff, Download, FileImage, Files, FileText, LoaderCircle, RefreshCw, Search } from 'lucide-react'
import { navigate, useSearch } from '@/app/navigation'
import { SpaceSwitcher } from '@/components/admin/space-switcher'
import { BrandMark } from '@/components/ui/brand-mark'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Field, Input, Label } from '@/components/ui/field'
import { fileBlob, fileBytes, fileImage, textFile } from '@/domain/file-content'
import { useLocale, useTranslate } from '@/i18n/provider'
import { useWorkbench } from '@/state/workbench'
import { fileFilters, fileKey, filesLocation, listFiles, readFileContent, type FileFilters, type FilePage, type SessionFile, type SessionFileContent } from './files-api'
import css from './files-page.module.css'
import { FilesNavigation } from './files-navigation'
import { fileInventoryKey, fileInventoryRevision, invalidateFileInventory, peekFileInventory, rememberFileInventory, subscribeFileInventory } from '@/domain/file-inventory'

const emptyFiles: FilePage = { items: [], next_cursor: null, offline_sources: [] }

function message(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause)
}

function FilePreview({ file, onClose, onDownload, downloading, downloadError }: {
  file: SessionFile
  onClose(): void
  onDownload(file: SessionFile, content?: SessionFileContent): void
  downloading: boolean
  downloadError: string
}) {
  const t = useTranslate('files')
  const [attempt, retry] = React.useReducer(value => value + 1, 0)
  const [state, setState] = React.useState<{ content: SessionFileContent | null; error: string; loading: boolean }>({ content: null, error: '', loading: true })
  const [imageFailed, setImageFailed] = React.useState(false)
  React.useEffect(() => {
    const controller = new AbortController()
    setState({ content: null, error: '', loading: true })
    setImageFailed(false)
    void readFileContent(file, controller.signal).then(content => {
      if (!controller.signal.aborted) setState({ content, error: '', loading: false })
    }).catch(cause => {
      if (!controller.signal.aborted) setState({ content: null, error: message(cause), loading: false })
    })
    return () => controller.abort()
  }, [file, attempt])
  const preview = React.useMemo(() => {
    if (!state.content) return null
    try {
      const image = fileImage(state.content)
      return image ? { image, text: null, error: '' }
        : textFile(state.content.media_type) ? { image: null, text: new TextDecoder().decode(fileBytes(state.content)), error: '' }
          : { image: null, text: null, error: '' }
    } catch (cause) {
      return { image: null, text: null, error: message(cause) }
    }
  }, [state.content])
  const error = state.error || preview?.error || (imageFailed ? t('imageError') : '')
  return <Dialog open onOpenChange={open => { if (!open) onClose() }}>
    <DialogContent className={css.previewDialog} showClose={false}>
      <DialogHeader>
        <DialogTitle className={css.previewTitle}>{file.name}</DialogTitle>
        <DialogDescription>{t('fileDetails', { kind: t(file.kind), workspace: file.workspace_name, session: file.session_title })}</DialogDescription>
      </DialogHeader>
      <div className={css.previewBody} data-file-preview="">
        {state.loading ? <div className={css.state} role="status"><LoaderCircle className={css.spinner} />{t('previewLoading')}</div>
          : error ? <div className={css.state} role="alert"><strong>{t('previewError')}</strong><p>{error}</p><Button variant="outline" onClick={retry}>{t('retry')}</Button></div>
            : preview?.image ? <img className={css.previewImage} src={preview.image} alt={file.name} onError={() => setImageFailed(true)} />
              : preview?.text !== null && preview?.text !== undefined ? <pre className={css.previewText} tabIndex={0} aria-label={t('textPreview')}>{preview.text}</pre>
                : <p className={css.state}>{t('unsupportedPreview')}</p>}
      </div>
      {downloadError && <p className={css.actionError} role="alert">{t('downloadError', { message: downloadError })}</p>}
      <div className={css.previewActions}>
        <Button variant="outline" onClick={onClose}>{t('close')}</Button>
        <Button disabled={downloading || state.loading} onClick={() => onDownload(file, state.content ?? undefined)}><Download />{t(downloading ? 'downloading' : 'download')}</Button>
      </div>
    </DialogContent>
  </Dialog>
}

function FileList({ filters }: { filters: FileFilters }) {
  const t = useTranslate('files')
  const { locale } = useLocale()
  const { snapshot, selectSession, accountScope } = useWorkbench()
  const cacheKey = fileInventoryKey(accountScope ?? 'local', filters)
  const revision = React.useSyncExternalStore(subscribeFileInventory, fileInventoryRevision)
  const [cached] = React.useState(() => peekFileInventory(cacheKey))
  const [cursor, setCursor] = React.useState<string | null>(cached?.cursor ?? null)
  const [attempt, retry] = React.useReducer(value => value + 1, 0)
  const [listing, setListing] = React.useState(() => ({ revision, page: cached?.page ?? emptyFiles }))
  const page = listing.revision === revision ? listing.page : emptyFiles
  const [loading, setLoading] = React.useState(!cached)
  const pageRef = React.useRef(page)
  pageRef.current = page
  const loadedRevision = React.useRef(revision)
  const [error, setError] = React.useState('')
  const [selected, setSelected] = React.useState<SessionFile | null>(null)
  const [downloading, setDownloading] = React.useState<string | null>(null)
  const [downloadError, setDownloadError] = React.useState<{ key: string; message: string } | null>(null)
  const downloadController = React.useRef<AbortController | null>(null)
  React.useEffect(() => () => downloadController.current?.abort(), [])
  React.useEffect(() => {
    if (loadedRevision.current !== revision) {
      loadedRevision.current = revision
      setSelected(null)
      if (cursor !== null) { setCursor(null); return }
    }
    const existing = peekFileInventory(cacheKey)
    if (existing && existing.cursor === cursor && attempt === 0) {
      setListing({ revision, page: existing.page })
      setLoading(false)
      return
    }
    const controller = new AbortController()
    setLoading(true)
    setError('')
    void listFiles(filters, cursor, controller.signal).then(result => {
      if (controller.signal.aborted || fileInventoryRevision() !== revision) return
      const previous = pageRef.current
      const next = cursor === null ? result : {
        ...result,
        items: [...previous.items, ...result.items],
        offline_sources: [...new Map([...previous.offline_sources, ...result.offline_sources].map(source => [JSON.stringify([source.workspace_id, source.executor_id]), source])).values()],
      }
      rememberFileInventory(cacheKey, next, cursor)
      setListing({ revision, page: next })
      setLoading(false)
    }).catch(cause => {
      if (!controller.signal.aborted && fileInventoryRevision() === revision) { setError(message(cause)); setLoading(false) }
    })
    return () => controller.abort()
  }, [filters, cursor, attempt, revision, cacheKey])

  const download = async (file: SessionFile, resolved?: SessionFileContent) => {
    downloadController.current?.abort()
    const controller = new AbortController()
    downloadController.current = controller
    const key = fileKey(file)
    setDownloading(key)
    setDownloadError(null)
    try {
      const content = resolved ?? await readFileContent(file, controller.signal)
      const blob = fileBlob(content)
      if (controller.signal.aborted) return
      const url = URL.createObjectURL(blob)
      const anchor = document.createElement('a')
      anchor.href = url
      anchor.download = content.name || file.name
      document.body.append(anchor)
      anchor.click()
      anchor.remove()
      window.setTimeout(() => URL.revokeObjectURL(url), 0)
    } catch (cause) {
      if (!controller.signal.aborted) setDownloadError({ key, message: message(cause) })
    } finally {
      if (!controller.signal.aborted) setDownloading(null)
    }
  }
  const canOpenSession = (file: SessionFile) => !file.session_archived && snapshot.sessions.some(session => session.identity.session_id === file.session_id && session.archived_at_ms == null)
  const openSource = (file: SessionFile) => {
    if (canOpenSession(file)) { selectSession(file.session_id); navigate('/') }
    else navigate(filesLocation({ session_id: file.session_id }))
  }
  const dates = React.useMemo(() => new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en', { dateStyle: 'medium', timeStyle: 'medium' }), [locale])
  const offlineNames = [...new Set(page.offline_sources.map(source => snapshot.workspaces.find(workspace => workspace.workspace_id === source.workspace_id)?.title || source.workspace_id))]
  return <div className={css.results} data-file-results="">
    {page.offline_sources.length > 0 && <div className={css.offlineNotice} role="status" data-files-offline=""><CloudOff /><div><p>{t('offlineNotice')}</p><p>{t('offlineWorkspaces', { names: offlineNames.join('、') })}</p></div></div>}
    {page.items.length > 0 && <p className={css.loadedCount}>{t('loadedCount', { count: page.items.length })}</p>}
    <ul className={css.fileList} aria-label={t('title')}>
      {page.items.map(file => {
        const key = fileKey(file)
        const Icon = file.media_type.startsWith('image/') ? FileImage : FileText
        return <li className={css.fileRow} key={key} data-file-id={file.id} data-file-session={file.session_id}>
          <span className={css.fileIcon}><Icon aria-hidden /></span>
          <div className={css.fileIdentity}>
            <button className={css.fileName} aria-label={t('preview', { name: file.name })} onClick={() => setSelected(file)}>{file.name}</button>
            {file.path && file.path !== file.name && <p className={css.filePath} title={file.path}>{file.path}</p>}
            <div className={css.fileMeta}><span>{file.workspace_name}</span><span>{file.session_title}</span><button onClick={() => openSource(file)}>{t(canOpenSession(file) ? 'openSession' : 'sessionFiles')}<ArrowUpRight /></button>{!canOpenSession(file) && <span>{t(file.session_archived ? 'archivedSession' : 'unloadedSession')}</span>}</div>
            <div className={css.fileMeta}><span className={css.kind}>{t(file.kind)}</span><time dateTime={new Date(file.occurred_at_ms).toISOString()}>{dates.format(file.occurred_at_ms)}</time>{file.kind === 'generated' && <span>{t('generatedRecord', { event: file.event_seq })}</span>}{file.source_status === 'offline' && <span className={css.offline}><CloudOff />{t('offline')}</span>}</div>
            {downloadError?.key === key && <p className={css.actionError} role="alert">{t('downloadError', { message: downloadError.message })}</p>}
          </div>
          <Button variant="ghost" size="icon" className={css.download} disabled={downloading !== null} aria-label={t('downloadFile', { name: file.name })} title={t('downloadFile', { name: file.name })} onClick={() => void download(file)}>{downloading === key ? <LoaderCircle className={css.spinner} /> : <Download />}</Button>
        </li>
      })}
    </ul>
    {loading && <div className={css.state} role="status"><LoaderCircle className={css.spinner} />{t('loading')}</div>}
    {error && <div className={css.state} role="alert"><strong>{t('loadError')}</strong><p>{error}</p><Button variant="outline" onClick={retry}>{t('retry')}</Button></div>}
    {!loading && !error && page.items.length === 0 && <div className={css.state} data-files-empty=""><Files /><strong>{t(page.offline_sources.length ? 'partialEmpty' : 'empty')}</strong>{!page.offline_sources.length && <p>{t('emptyDescription')}</p>}</div>}
    {!loading && !error && page.next_cursor && <div className={css.more}><Button variant="outline" onClick={() => setCursor(page.next_cursor)}>{t('loadMore')}</Button></div>}
    {selected && <FilePreview key={fileKey(selected)} file={selected} onClose={() => setSelected(null)} onDownload={(file, content) => void download(file, content)} downloading={downloading !== null} downloadError={downloadError?.key === fileKey(selected) ? downloadError.message : ''} />}
  </div>
}

function FileBrowser({ filters, mobile }: { filters: FileFilters; mobile: boolean }) {
  const t = useTranslate('files')
  const [query, setQuery] = React.useState(filters.query)
  React.useEffect(() => setQuery(filters.query), [filters.query])
  const change = (values: Partial<FileFilters>) => navigate(filesLocation({ ...filters, ...values }))
  return <>
    <div className={css.heading}><div><h1>{t('title')}</h1><p>{t('description')}</p></div><Button variant="outline" onClick={invalidateFileInventory}><RefreshCw />{t('refresh')}</Button></div>
    {mobile && <FilesNavigation filters={filters} mobile />}
    <form className={css.filters} onSubmit={event => { event.preventDefault(); change({ query: query.trim() }) }} aria-label={t('search')}>
      <Field className={css.searchField}><Label htmlFor="files-query">{t('search')}</Label><div className={css.searchControl}><Input id="files-query" type="search" value={query} placeholder={t('searchPlaceholder')} onChange={event => setQuery(event.target.value)} /><Button type="submit" variant="outline" aria-label={t('searchAction')}><Search /></Button></div></Field>
      {(filters.workspace_id || filters.session_id || filters.kind || filters.query) && <Button className={css.clear} variant="ghost" type="button" onClick={() => navigate('/files')}>{t('clearFilters')}</Button>}
    </form>
    <FileList key={JSON.stringify(filters)} filters={filters} />
  </>
}

function subscribeViewport(listener: () => void) {
  window.addEventListener('resize', listener)
  return () => window.removeEventListener('resize', listener)
}

export function FilesPage() {
  const mobile = React.useSyncExternalStore(subscribeViewport, () => window.innerWidth <= 760)
  const search = useSearch()
  const filters = React.useMemo(() => fileFilters(search), [search])
  const t = useTranslate('files')
  const { platform, accountScope, serverIdentity, currentTenantId, currentTenantRole, tenants, loading, error, authRequired, accessPaused, refresh } = useWorkbench()
  const scope = accountScope ?? 'local'
  const [readyScope, setReadyScope] = React.useState<string | null>(null)
  const permitted = !authRequired && !accessPaused && (!platform || Boolean(serverIdentity && currentTenantId && currentTenantRole))
  const ready = permitted && (!loading || readyScope === scope) && (!error || readyScope === scope)
  React.useEffect(() => { if (ready && !loading && !error) setReadyScope(scope) }, [ready, loading, error, scope])
  React.useEffect(() => { document.title = `${t('title')} · Ternilo` }, [t])
  return <div className={css.shell} data-files-shell="">
    <aside className={css.sidebar} aria-label={t('navigation')}>
      <a className={css.brand} href="/" onClick={event => { event.preventDefault(); navigate('/') }}><BrandMark /><strong>Ternilo</strong></a>
      {ready && !mobile && <FilesNavigation key={scope} filters={filters} />}
      <a className={css.backLink} href="/" onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft />{t('back')}</a>
    </aside>
    <div className={css.main}>
      <header className={css.header}><a className={css.mobileBack} href="/" aria-label={t('back')} onClick={event => { event.preventDefault(); navigate('/') }}><ArrowLeft /></a><span className={css.scope}>{platform ? serverIdentity?.user.username || '' : t('localScope')}</span>{platform && tenants.length > 0 && <div className={css.space}><SpaceSwitcher showCreate={false} /></div>}</header>
      <main className={css.content}>{ready ? <FileBrowser key={scope} filters={filters} mobile={mobile} /> : error && !loading && !authRequired ? <div className={css.state} role="alert"><strong>{t('bootstrapError')}</strong><p>{error}</p><Button variant="outline" onClick={() => void refresh().catch(() => undefined)}>{t('retry')}</Button></div> : <div className={css.state} role="status"><LoaderCircle className={css.spinner} />{t('loading')}</div>}</main>
    </div>
  </div>
}
