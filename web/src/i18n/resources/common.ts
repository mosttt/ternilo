import type { LocaleNamespaceMap } from '../runtime'

export const zh = {
  cancel: '取消',
  close: '关闭',
  copy: '复制',
  copied: '已复制',
  retry: '重试',
  loading: '加载中…',
  submit: '提交',
  delete: '删除',
  edit: '编辑',
  save: '保存',
  saving: '正在保存…',
  search: '搜索',
  collapse: '收起',
  expand: '展开',
} satisfies Record<string, string>

export type CommonKey = keyof typeof zh

export const en = {
  cancel: 'Cancel',
  close: 'Close',
  copy: 'Copy',
  copied: 'Copied',
  retry: 'Retry',
  loading: 'Loading…',
  submit: 'Submit',
  delete: 'Delete',
  edit: 'Edit',
  save: 'Save',
  saving: 'Saving…',
  search: 'Search',
  collapse: 'Collapse',
  expand: 'Expand',
} satisfies Record<CommonKey, string>

declare module '../runtime' {
  interface LocaleNamespaceMap {
    common: CommonKey
  }
}

void (null as LocaleNamespaceMap | null)
