import type { DirectoryEntry, DirectoryListing } from '@/types'

export function directorySeparator(listing: DirectoryListing): '/' | '\\' {
  return listing.home.includes('\\') ? '\\' : '/'
}

export function directoryWithSeparator(listing: DirectoryListing): string {
  const separator = directorySeparator(listing)
  return listing.path.endsWith(separator) ? listing.path : `${listing.path}${separator}`
}

export function directoryDraftParts(
  listing: DirectoryListing,
  draft: string,
  scanned?: { directory: string; landed: string } | null,
): { directory: string | null; prefix: string | null } {
  const cut = directorySeparator(listing) === '\\'
    ? Math.max(draft.lastIndexOf('\\'), draft.lastIndexOf('/'))
    : draft.lastIndexOf('/')
  if (cut < 0) return { directory: null, prefix: null }
  const directory = draft.slice(0, cut + 1)
  const canonicalLevel = directoryWithSeparator(listing)
  const answersDraft = directory === canonicalLevel
    || (scanned?.directory === directory && scanned.landed === listing.path)
  return { directory, prefix: answersDraft ? draft.slice(cut + 1) : null }
}

export function visibleDirectoryEntries(
  entries: readonly DirectoryEntry[],
  selectedPath: string | null,
  showHidden: boolean,
  prefix: string | null,
): readonly DirectoryEntry[] {
  const needle = prefix?.toLowerCase() ?? ''
  const canDisplay = (entry: DirectoryEntry) => showHidden || !entry.hidden || needle.startsWith('.')
  const matches = (entry: DirectoryEntry) => canDisplay(entry) && entry.name.toLowerCase().startsWith(needle)
  const narrows = needle.length > 0 && entries.some(matches)
  return entries.filter(entry => entry.path === selectedPath || (narrows ? matches(entry) : showHidden || !entry.hidden))
}

export function directoryParentPath(listing: DirectoryListing): string | null {
  return listing.crumbs.at(-2)?.path ?? null
}

export function displayDirectoryCrumbs(listing: DirectoryListing, homeLabel: string): DirectoryEntry[] {
  const homeIndex = listing.crumbs.findIndex(crumb => crumb.path === listing.home)
  if (homeIndex < 0) return listing.crumbs
  return [
    { name: homeLabel, path: listing.home, hidden: false },
    ...listing.crumbs.slice(homeIndex + 1),
  ]
}

/** Windows paths compare case-insensitively; POSIX paths remain byte-spelling sensitive. */
export function sameDirectoryPath(listing: DirectoryListing, left: string, right: string): boolean {
  return directorySeparator(listing) === '\\'
    ? left.toLowerCase() === right.toLowerCase()
    : left === right
}
