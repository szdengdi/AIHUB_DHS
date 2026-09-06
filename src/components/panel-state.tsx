import type { ReactNode } from 'react'
import { Spinner } from '@heroui/react'
import { useTranslation } from 'react-i18next'
import { If } from 'react-if-lite'

/**
 * 列表三态（加载中 / 加载失败 / 正常内容）。
 * 对齐各配置面板（plugin / core / profile）的展示：加载中带 spinner，
 * 失败态复用 plugins.error 文案；error 为空且非 loading 时渲染 children。
 */
export interface PanelStateProps {
  loading: boolean
  error: string
  children?: ReactNode
}

export function PanelState({ loading, error, children }: PanelStateProps) {
  const { t } = useTranslation()
  return (
    <If
      cond={!loading && error === ''}
      else={(
        <If
          cond={loading}
          else={(
            <p className="rounded-md border border-danger/30 bg-danger/5 p-3 text-xs text-danger">
              {t('plugins.error')}
              ：
              {error}
            </p>
          )}
        >
          <div className="flex items-center justify-center gap-2 p-4 text-xs text-muted">
            <Spinner size="sm" color="current" />
            {t('plugins.loading')}
          </div>
        </If>
      )}
    >
      {children}
    </If>
  )
}
