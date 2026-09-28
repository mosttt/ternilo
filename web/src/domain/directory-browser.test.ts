import { describe, expect, it } from 'vitest'
import type { DirectoryListing } from '@/types'
import {
  directoryDraftParts,
  directoryParentPath,
  directoryWithSeparator,
  displayDirectoryCrumbs,
  sameDirectoryPath,
  visibleDirectoryEntries,
} from './directory-browser'

const listing: DirectoryListing = {
  path: '/home/move/project',
  home: '/home/move',
  crumbs: [
    { name: '/', path: '/', hidden: false },
    { name: 'home', path: '/home', hidden: false },
    { name: 'move', path: '/home/move', hidden: false },
    { name: 'project', path: '/home/move/project', hidden: false },
  ],
  entries: [
    { name: '.git', path: '/home/move/project/.git', hidden: true },
    { name: 'crates', path: '/home/move/project/crates', hidden: false },
    { name: 'docs', path: '/home/move/project/docs', hidden: false },
  ],
  truncated: false,
}

describe('directory browser domain', () => {
  it('derives the directory and final prefix without joining host paths', () => {
    expect(directoryDraftParts(listing, '/home/move/project/do')).toEqual({
      directory: '/home/move/project/', prefix: 'do',
    })
    expect(directoryDraftParts(listing, '/home/move/other/do')).toEqual({
      directory: '/home/move/other/', prefix: null,
    })
  })

  it('narrows matching prefixes, reveals explicit dot prefixes and releases misses', () => {
    expect(visibleDirectoryEntries(listing.entries, null, false, 'd').map(entry => entry.name)).toEqual(['docs'])
    expect(visibleDirectoryEntries(listing.entries, null, false, '.g').map(entry => entry.name)).toEqual(['.git'])
    expect(visibleDirectoryEntries(listing.entries, null, false, 'missing').map(entry => entry.name)).toEqual(['crates', 'docs'])
    expect(visibleDirectoryEntries(listing.entries, '/home/move/project/.git', false, null).map(entry => entry.name)).toContain('.git')
  })

  it('roots display crumbs at home while retaining the real parent path', () => {
    expect(displayDirectoryCrumbs(listing, 'Home').map(crumb => crumb.name)).toEqual(['Home', 'project'])
    expect(directoryParentPath(listing)).toBe('/home/move')
  })

  it('preserves platform spelling rules without interpreting POSIX backslashes', () => {
    expect(directoryWithSeparator(listing)).toBe('/home/move/project/')
    expect(directoryDraftParts(listing, String.raw`folder\name`)).toEqual({ directory: null, prefix: null })
    expect(sameDirectoryPath(listing, '/A', '/a')).toBe(false)

    const windows = { ...listing, home: String.raw`C:\Users\move`, path: String.raw`C:\Users` }
    expect(directoryWithSeparator(windows)).toBe('C:\\Users\\')
    expect(directoryDraftParts(windows, 'C:/Us')).toEqual({ directory: 'C:/', prefix: null })
    expect(sameDirectoryPath(windows, String.raw`C:\USERS`, String.raw`c:\users`)).toBe(true)
  })
})
