import type { ReactNode } from "react"
import { cn } from "@/lib/utils"

export type PillKind = "accent" | "slate"

export function Pill({
  kind = "slate",
  children,
  className,
}: {
  kind?: PillKind
  children: ReactNode
  className?: string
}) {
  return (
    <span className={cn("pill", `pill--${kind}`, className)}>{children}</span>
  )
}

/** Inherits its colour from the parent. */
export function LiveDot() {
  return <span className="dot-live" />
}
