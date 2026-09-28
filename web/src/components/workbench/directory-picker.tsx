import { useWorkbench } from '@/state/workbench'
import { LocalDirectoryPicker } from './local-directory-picker'
import { PlatformWorkspacePicker } from './platform-workspace-picker'

export function DirectoryPicker(props: {
  open: boolean
  onOpenChange(open: boolean): void
  createSessionAfter?: boolean
}) {
  const { platform, currentTenantRole } = useWorkbench()
  if (platform && (currentTenantRole === null || currentTenantRole === 'viewer')) return null
  return platform ? <PlatformWorkspacePicker {...props} /> : <LocalDirectoryPicker {...props} />
}
