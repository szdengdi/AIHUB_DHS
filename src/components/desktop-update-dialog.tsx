import type { PropsWithOverlays } from '@overlastic/react'
import { useWatch } from '@hairy/react-lib'
import { AlertDialog, Button, Description, ProgressBar } from '@heroui/react'
import { useDisclosure } from '@overlastic/react'
import { useRef } from 'react'
import { useTranslation } from 'react-i18next'
import { If } from 'react-if-lite'
import { useStore } from 'valtio-define'
import { store } from '@/store'
import { Info } from './info'

export interface DesktopUpdateDialogProps extends PropsWithOverlays {}

/**
 * 「检查更新」对话框：展示新版本信息 + 下载进度。
 * 已下载 → 「打开安装包」直接启动安装器；未下载 → 「立即更新」下载完成后自动打开。
 *
 * 使用 overlastic 命令式打开（`useOverlay(DesktopUpdateDialog)`）。
 * 下载由 store 驱动：对话框内「立即更新」触发 `downloadAndOpen`；
 * 外部触发（右下角 toast）下载完成并打开安装包后，对话框依据 downloading 回落自动关闭。
 */
export function DesktopUpdateDialog(props: DesktopUpdateDialogProps) {
  const disclosure = useDisclosure({ props })
  const { t } = useTranslation()
  const { updateInfo, downloading, downloadProgress } = useStore(store.desktopUpdater)
  const prevDownloadingRef = useRef(false)

  // 外部触发下载（如右下角 toast「立即更新」）完成并打开安装包后，自动关闭对话框；
  // 仅当 downloading 由 true→false 且已下载（安装包已打开）时才关闭，下载失败保持打开。
  useWatch([downloading, updateInfo], () => {
    if (prevDownloadingRef.current && !downloading && updateInfo?.downloaded)
      disclosure.cancel()
    prevDownloadingRef.current = downloading
  }, { immediate: true })

  async function handlePrimary() {
    const info = store.desktopUpdater.updateInfo
    if (!info)
      return
    if (info.downloaded) {
      await store.desktopUpdater.openInstaller(info.path)
      disclosure.cancel()
    }
    else {
      await store.desktopUpdater.downloadAndOpen()
    }
  }

  return (
    <AlertDialog onOpenChange={disclosure.cancel} isOpen={disclosure.visible}>
      <AlertDialog.Backdrop isDismissable={!downloading}>
        <AlertDialog.Container>
          <AlertDialog.Dialog className="sm:max-w-[420px]">
            <AlertDialog.CloseTrigger isDisabled={downloading} />
            <AlertDialog.Header>
              <AlertDialog.Icon status="default" />
              <AlertDialog.Heading>{t('update.desktop_title')}</AlertDialog.Heading>
            </AlertDialog.Header>
            <AlertDialog.Body className="space-y-3">
              <If cond={updateInfo != null}>
                <div className="space-y-1.5">
                  <Info term={t('ui.current_version')}>{updateInfo?.currentVersion}</Info>
                  <Info term={t('update.new_version_label')}>{updateInfo?.version}</Info>
                  <If cond={updateInfo?.downloaded}>
                    <Description className="text-xs">
                      {t('update.desktop_downloaded')}
                    </Description>
                  </If>
                </div>
              </If>

              <If cond={downloading}>
                <div className="space-y-1">
                  <div className="flex justify-between text-xs text-muted">
                    <span>{t('update.desktop_downloading')}</span>
                    <span className="shrink-0">
                      {Math.round(downloadProgress)}
                      %
                    </span>
                  </div>
                  <ProgressBar value={downloadProgress} className="w-full">
                    <ProgressBar.Track>
                      <ProgressBar.Fill className="bg-accent" />
                    </ProgressBar.Track>
                  </ProgressBar>
                </div>
              </If>
            </AlertDialog.Body>
            <AlertDialog.Footer>
              <Button
                variant="tertiary"
                className="rounded-md"
                isDisabled={downloading}
                onPress={disclosure.cancel}
              >
                {t('update.later')}
              </Button>
              <Button
                variant="primary"
                className="rounded-md"
                isDisabled={downloading || updateInfo == null}
                onPress={handlePrimary}
              >
                {updateInfo?.downloaded
                  ? t('update.open_installer')
                  : t('update.now')}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}
