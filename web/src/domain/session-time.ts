export function sessionRelativeTime(timestamp: number, now = Date.now(), locale: 'zh' | 'en' = 'zh') {
  const elapsed = Math.max(0, now - timestamp)
  const minutes = Math.floor(elapsed / 60_000)
  if (minutes < 1) return locale === 'zh' ? '刚刚' : 'now'
  if (minutes < 60) return locale === 'zh' ? `${minutes}分钟` : `${minutes}min`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return locale === 'zh' ? `${hours}小时` : `${hours}h`
  const days = Math.floor(hours / 24)
  if (days < 30) return locale === 'zh' ? `${days}天` : `${days}d`
  if (days < 365) {
    const months = Math.floor(days / 30)
    return locale === 'zh' ? `${months}个月` : `${months}mo`
  }
  const years = Math.floor(days / 365)
  return locale === 'zh' ? `${years}年` : `${years}y`
}

export function sessionAbsoluteTime(timestamp: number, locale: 'zh' | 'en' = 'zh') {
  return new Intl.DateTimeFormat(locale === 'zh' ? 'zh-CN' : 'en-US', {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
  }).format(timestamp)
}
