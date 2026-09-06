import type { ReactNode } from 'react'
import type { VariantProps } from 'tailwind-variants'
import { Tooltip } from '@heroui/react'
import { useRef, useState } from 'react'
import { cn, tv } from 'tailwind-variants'

export interface EllipsisProps {
  lineClamp?: number
  tooltip?: ReactNode
  children?: ReactNode
  className?: string
  tooltipClassName?: string
  forceTooltip?: boolean
}

const ellipsis = tv({
  slots: {
    container: '',
    tooltip: '',
  },
})

export function Ellipsis(props: EllipsisProps & VariantProps<typeof ellipsis>) {
  const [open, setOpen] = useState(false)
  const { container, tooltip } = ellipsis(props)
  const triggerRef = useRef<HTMLDivElement>(null)
  const triggerInnerRef = useRef<HTMLSpanElement>(null)

  function getDisabled() {
    if (!triggerRef.current)
      return true
    let tooltipDisabled = false
    const { current: trigger } = triggerRef
    const { current: triggerInner } = triggerInnerRef
    if (props.lineClamp !== undefined) {
      tooltipDisabled = trigger.scrollHeight <= trigger.offsetHeight
    }
    else if (triggerInner) {
      tooltipDisabled = triggerInner.getBoundingClientRect().width <= trigger.getBoundingClientRect().width
    }
    return tooltipDisabled
  }

  function onOpenChange(open: boolean) {
    if (getDisabled() && !props.forceTooltip)
      return

    setOpen(open)
  }
  return (
    <Tooltip isOpen={open} onOpenChange={onOpenChange}>
      <div
        ref={triggerRef}
        className={cn(
          props.lineClamp === undefined ? 'truncate' : 'line-clamp-[var(--line-clamp)] [display:-webkit-inline-box]!',
          container({ className: props.className }),
        )}
        style={
          {
            '--line-clamp': props.lineClamp,
          } as React.CSSProperties
        }
      >
        {props.lineClamp ? props.children : <span ref={triggerInnerRef}>{props.children}</span>}
      </div>
      <Tooltip.Content className={cn(tooltip(), props.tooltipClassName)}>
        {props.tooltip || props.children}
      </Tooltip.Content>
    </Tooltip>
  )
}
