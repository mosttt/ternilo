import { expect, it } from 'vitest'
import { workspaceImagePath } from './workspace-image'

it('resolves relative, placeholder, POSIX and Windows images within the bound workspace', () => {
  for (const source of ['figures/map.png', './figures/map.png', '<local-workspace>/figures/map.png', 'file://<local-workspace>/figures/map.png', 'file:///%3Clocal-workspace%3E/figures/map.png', '/home/projects/figures/map.png', 'file:///home/projects/figures/map.png']) {
    expect(workspaceImagePath(source, '/home/projects')).toBe('figures/map.png')
  }
  expect(workspaceImagePath('C:\\Projects\\map.png', 'c:\\projects')).toBe('map.png')
  expect(workspaceImagePath('file:///C:/Projects/map.png', 'C:\\Projects')).toBe('map.png')
  expect(workspaceImagePath('file:///C:/Projects/map.png', '\\\\?\\C:\\Projects')).toBe('map.png')
  expect(workspaceImagePath('\\\\?\\C:\\Projects\\map.png', 'C:\\Projects')).toBe('map.png')
  expect(workspaceImagePath('C:/Projects-other/map.png', '\\\\?\\C:\\Projects')).toBeNull()
  expect(workspaceImagePath('figures/a%20b.png', '/project')).toBe('figures/a b.png')
})
it('never converts other workspaces, traversal or external destinations into local reads', () => {
  for (const source of ['/home/project-secret/x.png', '/etc/passwd', '../private.png', '%2e%2e/private.png', 'a/../b.png', 'https://example.test/x.png', '//example.test/x.png', 'file://remote/home/project/x.png', 'javascript:alert(1)', 'data:image/svg+xml,test', '%00.png']) {
    expect(workspaceImagePath(source, '/home/project')).toBeNull()
  }
})
