import { useEffect, useState, lazy, Suspense } from "react"
import { useSearchParams } from "react-router-dom"
import {
  RefreshCw,
  Stethoscope,
  Gauge,
  Unplug,
  RotateCcw,
  Loader2,
} from "lucide-react"
import { api } from "@/lib/api"
import { HeaderStrip } from "@/components/settings/HeaderStrip"
import { ActionsRail, type ActionChipProps } from "@/components/settings/ActionsRail"
import { TabBar } from "@/components/ui/TabBar"
import { useVersion } from "@/hooks/useVersion"
import type { RawConfigEntry } from "@/components/settings/sections/RawConfigEditor"
import type { PiStatus } from "@/lib/api"

const DeviceTab = lazy(() => import("@/pages/settings/DeviceTab").then(m => ({ default: m.DeviceTab })))
const NetworkTab = lazy(() => import("@/pages/settings/NetworkTab").then(m => ({ default: m.NetworkTab })))
const NotificationsTab = lazy(() => import("@/pages/settings/NotificationsTab").then(m => ({ default: m.NotificationsTab })))
const SystemTab = lazy(() => import("@/pages/settings/SystemTab").then(m => ({ default: m.SystemTab })))

const SetupWizard = lazy(() => import("@/components/setup/SetupWizard").then(m => ({ default: m.SetupWizard })))
const RawConfigEditor = lazy(() => import("@/components/settings/sections/RawConfigEditor").then(m => ({ default: m.RawConfigEditor })))
const HealthCheckModal = lazy(() => import("@/components/settings/sections/HealthCheckModal").then(m => ({ default: m.HealthCheckModal })))
const SpeedTestModal = lazy(() => import("@/components/settings/sections/SpeedTestModal").then(m => ({ default: m.SpeedTestModal })))

const TABS = [
  "Device",
  "Car & Network",
  "Notifications",
  "System",
] as const
type TabName = (typeof TABS)[number]

function isTab(s: string | null): s is TabName {
  return !!s && (TABS as readonly string[]).includes(s)
}

export default function Settings() {
  const [params, setParams] = useSearchParams()
  const activeTab: TabName = isTab(params.get("tab")) ? (params.get("tab") as TabName) : "Device"

  const [status, setStatus] = useState<PiStatus | null>(null)
  const [piConfig, setPiConfig] = useState<{
    uses_ble?: string
    DASHUSB_HOSTNAME?: string
  } | null>(null)
  const [confirmReboot, setConfirmReboot] = useState(false)
  const [drivesConnected, setDrivesConnected] = useState<boolean | null>(null)
  // Advance uptime locally between status polls.
  const [tickOffset, setTickOffset] = useState(0)

  const [wizardOpen, setWizardOpen] = useState(false)
  const [wizardInitialData, setWizardInitialData] = useState<
    Record<string, string> | undefined
  >(undefined)
  const [rawConfigOpen, setRawConfigOpen] = useState(false)
  const [rawConfig, setRawConfig] = useState<Record<string, RawConfigEntry> | null>(null)
  const [healthOpen, setHealthOpen] = useState(false)
  const [speedOpen, setSpeedOpen] = useState(false)

  // Include the producing version in exported configuration.
  const version = useVersion()

  useEffect(() => {
    let mounted = true
    async function poll() {
      try {
        const data = await api.getStatus()
        if (mounted) {
          setStatus(data)
          setDrivesConnected(data.drives_active === "yes")
          setTickOffset(0)
        }
      } catch {
        /* ignore */
      }
    }
    poll()
    const id = setInterval(poll, 4000)
    const tickId = setInterval(() => setTickOffset((t) => t + 1), 1000)
    return () => {
      mounted = false
      clearInterval(id)
      clearInterval(tickId)
    }
  }, [])

  // Model comes from status; RTC flags do not identify battery-less Pi 5s.
  useEffect(() => {
    fetch("/api/config")
      .then((r) => r.json())
      .then((data) => setPiConfig(data))
      .catch(() => {})
  }, [])

  const sbc = status?.sbc_model || null
  // Configuration values support both wrapped and legacy raw shapes.
  const hostnameEntry = piConfig?.DASHUSB_HOSTNAME as
    | { value?: string; active?: boolean }
    | string
    | undefined
  const hostname =
    typeof hostnameEntry === "string"
      ? hostnameEntry
      : hostnameEntry?.active
      ? hostnameEntry.value || null
      : null

  function setTab(next: TabName) {
    const p = new URLSearchParams(params)
    p.set("tab", next)
    setParams(p, { replace: true })
  }

  async function handleReboot(): Promise<string | void> {
    // The first press arms the label-based confirmation without a success flash.
    if (!confirmReboot) {
      setConfirmReboot(true)
      setTimeout(() => setConfirmReboot(false), 10000)
      return "confirm"
    }
    const res = await fetch("/api/system/reboot", { method: "POST" })
    setConfirmReboot(false)
    if (!res.ok) throw new Error("Reboot failed")
    return "Rebooting…"
  }

  async function handleToggleDrives(): Promise<string> {
    const res = await fetch("/api/system/toggle-drives", { method: "POST" })
    if (!res.ok) throw new Error("Toggle failed")
    // Refresh the toggle label immediately.
    try {
      const data = await api.getStatus()
      setDrivesConnected(data.drives_active === "yes")
    } catch {
      /* non-critical */
    }
    return "Toggled"
  }

  async function handleArchiveSync(): Promise<string> {
    const res = await fetch("/api/system/trigger-sync", { method: "POST" })
    if (!res.ok) throw new Error("Sync failed")
    return "Triggered"
  }

  async function handleOpenRawConfig() {
    try {
      const res = await fetch("/api/setup/config")
      if (!res.ok) return
      const data = await res.json()
      setRawConfig(data)
      setRawConfigOpen(true)
    } catch {
      /* ignore */
    }
  }

  async function handleOpenWizard() {
    try {
      const res = await fetch("/api/setup/config")
      if (res.ok) {
        const data = await res.json()
        const flat: Record<string, string> = {}
        for (const [k, v] of Object.entries(data)) {
          const entry = v as { value: string; active: boolean }
          if (entry.active) flat[k] = entry.value
        }
        setWizardInitialData(flat)
      }
    } catch {
      /* ignore */
    }
    setWizardOpen(true)
  }

  const actions: ActionChipProps[] = [
    {
      icon: RefreshCw,
      label: "Archive Sync",
      onClick: handleArchiveSync,
    },
    {
      icon: Stethoscope,
      label: "Health Check",
      onClick: () => setHealthOpen(true),
    },
    {
      icon: Gauge,
      label: "Speed Test",
      onClick: () => setSpeedOpen(true),
    },
    {
      icon: Unplug,
      label:
        drivesConnected === null
          ? "Toggle USB"
          : drivesConnected
          ? "USB · Connected"
          : "USB · Disconnected",
      onClick: handleToggleDrives,
    },
  ]
  const dangerActions: ActionChipProps[] = [
    {
      icon: RotateCcw,
      label: confirmReboot ? "Confirm Restart" : "Restart Pi",
      variant: "danger",
      onClick: handleReboot,
    },
  ]

  const uptimeSec = status ? parseFloat(status.uptime) + tickOffset : null

  const [isMobile, setIsMobile] = useState(
    typeof window !== "undefined" && window.innerWidth < 640
  )
  useEffect(() => {
    const onResize = () => setIsMobile(window.innerWidth < 640)
    window.addEventListener("resize", onResize)
    return () => window.removeEventListener("resize", onResize)
  }, [])

  return (
    <div className="space-y-3">
      <HeaderStrip
        hostname={hostname}
        sbc={sbc}
        uptimeSec={uptimeSec}
      />

      <ActionsRail actions={actions} danger={dangerActions} />

      <TabBar tabs={TABS} active={activeTab} onSelect={setTab} scrollable={isMobile} />

      <Suspense fallback={<TabFallback />}>
        {activeTab === "Device" && <DeviceTab />}
        {activeTab === "Car & Network" && (
          <NetworkTab status={status} />
        )}
        {activeTab === "Notifications" && <NotificationsTab />}
        {activeTab === "System" && (
          <SystemTab
            onOpenRawConfig={handleOpenRawConfig}
            onOpenWizard={handleOpenWizard}
            version={version}
            hostname={hostname}
          />
        )}
      </Suspense>

      {/* Load modal chunks independently. */}
      {wizardOpen && (
        <Suspense fallback={null}>
          <SetupWizard
            initialData={wizardInitialData}
            onClose={() => {
              setWizardOpen(false)
              setWizardInitialData(undefined)
            }}
          />
        </Suspense>
      )}
      {rawConfigOpen && rawConfig && (
        <Suspense fallback={null}>
          <RawConfigEditor
            config={rawConfig}
            onClose={() => {
              setRawConfigOpen(false)
              setRawConfig(null)
            }}
          />
        </Suspense>
      )}
      {healthOpen && (
        <Suspense fallback={null}>
          <HealthCheckModal onClose={() => setHealthOpen(false)} />
        </Suspense>
      )}
      {speedOpen && (
        <Suspense fallback={null}>
          <SpeedTestModal onClose={() => setSpeedOpen(false)} />
        </Suspense>
      )}
    </div>
  )
}

function TabFallback() {
  return (
    <div className="flex h-32 items-center justify-center">
      <Loader2 className="h-5 w-5 animate-spin text-slate-500" />
    </div>
  )
}
