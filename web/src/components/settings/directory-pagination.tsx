import { ChevronLeft, ChevronRight } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { useTranslate } from '@/i18n/provider'
import styles from './platform-settings.module.css'

export function DirectoryPagination({ page, count, loading, nextCursor, onPrevious, onNext }: {
  page: number
  count: number
  loading: boolean
  nextCursor: string | null
  onPrevious(): void
  onNext(cursor: string): void
}) {
  const t = useTranslate('settings')
  return <div className={styles.pagination}>
    <span>{t('directory.page', { page, count })}</span>
    <div>
      <Button type="button" variant="outline" disabled={loading || page === 1} onClick={onPrevious}><ChevronLeft />{t('directory.previous')}</Button>
      <Button type="button" variant="outline" disabled={loading || !nextCursor} onClick={() => { if (nextCursor) onNext(nextCursor) }}>{t('directory.next')}<ChevronRight /></Button>
    </div>
  </div>
}
