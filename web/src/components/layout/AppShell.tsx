import { useState, useEffect } from "react"
import { Outlet } from "react-router-dom"
import { Menu } from "lucide-react"
import { Sidebar } from "./Sidebar"
import { MobileNav } from "./MobileNav"
import { ConnectionBanner } from "./ConnectionBanner"
import { cn } from "@/lib/utils"
import { ConnectionProvider } from "@/hooks/useConnectionStatus"

// Routes likely to follow the Dashboard, prefetched on idle so navigation is
// instant. Heavy or rarely opened routes (Terminal) stay out of this list:
// don't burn data on screens the user may never visit.
const PREFETCH_ROUTES: Array<() => Promise<unknown>> = [
  () => import("@/pages/Viewer"),
  () => import("@/pages/Files"),
  () => import("@/pages/Settings"),
  () => import("@/pages/Logs"),
]

export function AppShell() {
  // Persist the collapse choice across reloads.
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => {
    try {
      return localStorage.getItem("sidebar-collapsed") === "1"
    } catch {
      return false
    }
  })
  const [mobileNavOpen, setMobileNavOpen] = useState(false)

  // Warm the module cache for likely-next routes on idle. Skipped under
  // Save-Data or a slow effective connection so metered users aren't charged
  // for pages they never open.
  useEffect(() => {
    const conn = (navigator as Navigator & {
      connection?: { saveData?: boolean; effectiveType?: string }
    }).connection
    if (conn?.saveData) return
    if (conn?.effectiveType === "slow-2g" || conn?.effectiveType === "2g") return

    const idle = (window as Window & {
      requestIdleCallback?: (cb: () => void, opts?: { timeout: number }) => number
    }).requestIdleCallback ?? ((cb: () => void) => setTimeout(cb, 1500))

    const handle = idle(() => {
      PREFETCH_ROUTES.forEach((load) => { load().catch(() => {}) })
    }, { timeout: 3000 })

    return () => {
      const cancel = (window as Window & {
        cancelIdleCallback?: (handle: number) => void
      }).cancelIdleCallback
      if (cancel && typeof handle === "number") cancel(handle)
    }
  }, [])

  return (
    <ConnectionProvider>
        <div className="flex h-full">
          {/* Desktop sidebar */}
          <div className="hidden md:block">
            <Sidebar
              collapsed={sidebarCollapsed}
              onToggle={() =>
                setSidebarCollapsed((c) => {
                  const next = !c
                  try {
                    localStorage.setItem("sidebar-collapsed", next ? "1" : "0")
                  } catch {
                    /* ignore */
                  }
                  return next
                })
              }
            />
          </div>

          {/* Mobile nav drawer */}
          <MobileNav open={mobileNavOpen} onClose={() => setMobileNavOpen(false)} />

          {/* Main content */}
          <main
            className={cn(
              "flex-1 overflow-y-auto transition-all duration-300",
              "md:ml-56",
              sidebarCollapsed && "md:ml-16"
            )}
          >
            {/* Mobile header */}
            <div className="sticky top-0 z-[500] flex h-14 items-center gap-3 border-b border-white/5 bg-slate-950/80 px-4 backdrop-blur-md md:hidden">
              <button
                onClick={() => setMobileNavOpen(true)}
                className="rounded-lg p-2 text-slate-400 hover:bg-white/5 hover:text-slate-200"
              >
                <Menu className="h-5 w-5" />
              </button>
              <span className="text-sm font-semibold text-slate-100" style={{ fontFamily: '"Inter", -apple-system, system-ui, sans-serif' }}>Dash USB</span>
            </div>

            {/* Cap and center the content column in the post-sidebar area so
                cards don't stretch into sparse bands on wide monitors or
                zoomed-out viewports. Pages that genuinely need full bleed can
                opt out with their own wrapper. */}
            <div className="mx-auto w-full max-w-[1280px] p-4 pb-safe md:p-6">
              <ConnectionBanner />
              <Outlet />
            </div>
          </main>
        </div>
    </ConnectionProvider>
  )
}
