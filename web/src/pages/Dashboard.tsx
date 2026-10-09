import { useEffect, useState } from "react"
import { Link } from "react-router-dom"
import {
  Thermometer,
  HardDrive,
  Wifi,
  WifiOff,
  Clock,
  Camera,
  Activity,
  EthernetPort,
  Zap,
  ChevronRight,
  Download,
  AlertTriangle,
  Wind,
  Info,
} from "lucide-react"
import { api } from "@/lib/api"
import { useUpdateAvailable } from "@/hooks/useUpdateAvailable"
import { useWifiFirmware } from "@/hooks/useWifiFirmware"
import { WifiFirmwareModal } from "@/components/dashboard/WifiFirmwareModal"
import type { PiStatus, StorageBreakdown, ArchiveStatus } from "@/lib/api"
import { formatUptime, formatBytes, formatTemp } from "@/lib/utils"
import { useUnits } from "@/lib/units"
import { StatusTile, Row, TileDivider } from "@/components/ui/StatusTile"
import { BannerStack, type BannerItem } from "@/components/ui/Banner"
import { Pill, LiveDot } from "@/components/ui/Pill"
import type { Halo } from "@/components/ui/StatusTile"

function getTempHalo(milliC: number): Halo {
  if (milliC <= 0) return "blue"
  if (milliC < 55000) return "accent"
  if (milliC < 70000) return "amber"
  return "red"
}

function getTempColor(milliC: number): string {
  if (milliC < 55000) return "oklch(0.78 0.14 240)"
  if (milliC < 70000) return "#fbbf24"
  return "#f87171"
}

function formatThroughput(bps: number): string {
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`
  if (bps >= 1_000) return `${Math.round(bps / 1_000)} Kbps`
  return bps > 0 ? "< 1 Kbps" : "—"
}

function getWifiStrengthBars(strength: string): number {
  if (!strength) return 0
  const parts = strength.split("/")
  if (parts.length !== 2) return 0
  const ratio = parseInt(parts[0]) / parseInt(parts[1])
  if (ratio > 0.75) return 4
  if (ratio > 0.5) return 3
  if (ratio > 0.25) return 2
  return 1
}

function WifiBars({ bars }: { bars: number }) {
  return (
    <span className="inline-flex items-end gap-[2px] align-middle" aria-label={`${bars}/4 bars`}>
      {[1, 2, 3, 4].map((n) => (
        <span
          key={n}
          className={n <= bars ? "bg-emerald-400" : "bg-slate-700"}
          style={{ width: 3, height: 3 + n * 2, borderRadius: 1 }}
        />
      ))}
    </span>
  )
}

export default function Dashboard() {
  const [status, setStatus] = useState<PiStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [uptime, setUptime] = useState(0)
  const [storageBreakdown, setStorageBreakdown] =
    useState<StorageBreakdown | null>(null)
  const [archiveStatus, setArchiveStatus] = useState<ArchiveStatus | null>(null)
  const [archiveFresh, setArchiveFresh] = useState(false)
  const [cancelPending, setCancelPending] = useState(false)
  const [cancelError, setCancelError] = useState("")
  const wifiFirmware = useWifiFirmware()
  const [wifiFwOpen, setWifiFwOpen] = useState(false)
  // Share live unit selection with Settings.
  const { tempF: systemUseFahrenheit } = useUnits()
  const [rtcWarning, setRtcWarning] = useState<string | null>(null)

  const updateInfo = useUpdateAvailable()

  useEffect(() => {
    let mounted = true

    async function fetchStatus() {
      try {
        const data = await api.getStatus()
        if (!mounted) return
        setStatus(data)
        setUptime(parseFloat(data.uptime))
        setError(null)
      } catch {
        if (mounted) setError("Unable to connect to Dash USB")
      }
    }

    async function fetchArchiveStatus() {
      try {
        const d: ArchiveStatus = await api.getArchiveStatus()
        if (!mounted) return
        setArchiveStatus(d)
        setArchiveFresh(true)
      } catch {
        if (mounted) setArchiveFresh(false)
      }
    }

    async function fetchStorageBreakdown() {
      try {
        const data = await api.getStorageBreakdown()
        if (mounted) setStorageBreakdown(data)
      } catch {
        /* non-critical */
      }
    }

    fetchStatus()
    fetchArchiveStatus()
    fetchStorageBreakdown()

    fetch("/api/system/rtc-status")
      .then((r) => r.json())
      .then((rtc) => {
        if (mounted && rtc.is_pi5 && !rtc.rtc_healthy && rtc.battery_warning) {
          setRtcWarning(rtc.battery_warning)
        }
      })
      .catch(() => {})

    // Pause network polling while the page is hidden.
    const statusInterval = setInterval(() => {
      if (!document.hidden) fetchStatus()
    }, 2000)
    const archiveInterval = setInterval(() => {
      if (!document.hidden) fetchArchiveStatus()
    }, 5000)
    const storageInterval = setInterval(() => {
      if (!document.hidden) fetchStorageBreakdown()
    }, 10000)
    // Avoid background renders for the local uptime counter.
    const uptimeInterval = setInterval(() => {
      if (!document.hidden) setUptime((p) => p + 1)
    }, 1000)

    // Refresh immediately when the tab becomes visible.
    const onVisible = () => {
      if (document.hidden) return
      fetchStatus()
      fetchArchiveStatus()
      fetchStorageBreakdown()
    }
    document.addEventListener("visibilitychange", onVisible)

    return () => {
      mounted = false
      clearInterval(statusInterval)
      clearInterval(archiveInterval)
      clearInterval(storageInterval)
      clearInterval(uptimeInterval)
      document.removeEventListener("visibilitychange", onVisible)
    }
  }, [])

  async function cancelArchive() {
    const cycle = archiveStatus?.cycle
    if (!cycle || cycle.cancelling || cancelPending || !archiveFresh) return
    setCancelPending(true)
    setCancelError("")
    try {
      await api.cancelArchive(cycle.id)
      setArchiveStatus(previous => previous?.cycle?.id === cycle.id
        ? { ...previous, cycle: { ...previous.cycle, cancelling: true } }
        : previous)
    } catch (cause) {
      setCancelError(cause instanceof Error ? cause.message : "Could not cancel this archive.")
    } finally {
      setCancelPending(false)
    }
  }

  if (error && !status) {
    return (
      <div className="flex flex-col items-center justify-center py-20">
        <Activity className="mb-4 h-12 w-12 text-slate-600" />
        <p className="text-lg font-medium text-slate-400">{error}</p>
        <p className="mt-1 text-sm text-slate-600">
          Make sure the Dash USB API server is running
        </p>
      </div>
    )
  }

  if (!status) {
    return (
      <div className="space-y-4">
        <h1 className="text-2xl font-bold text-slate-100">Dashboard</h1>
        <div className="tile-grid">
          {[...Array(4)].map((_, i) => (
            <div key={i} className="glass-card h-32 animate-pulse" />
          ))}
        </div>
      </div>
    )
  }

  const banners: BannerItem[] = []
  if (rtcWarning) {
    banners.push({
      id: "rtc",
      kind: "warn",
      icon: <AlertTriangle className="h-4 w-4" />,
      title: "RTC Battery Warning",
      sub: rtcWarning,
    })
  }
  if (wifiFirmware.installing || wifiFirmware.show || wifiFirmware.offerRevert || wifiFirmware.status?.reboot_pending) {
    const fw = wifiFirmware.status
    banners.push({
      id: "wifi-firmware",
      kind: wifiFirmware.show || fw?.reboot_pending ? "warn" : "update",
      icon: <Wifi className="h-4 w-4" />,
      title: wifiFirmware.installing
        ? `Updating Wi-Fi firmware… ${fw?.install.progress ?? 0}%`
        : fw?.reboot_pending ? "Wi-Fi firmware updated · restart pending"
        : wifiFirmware.show ? (fw?.symptom_detected ? "Wi-Fi bus errors detected" : "Wi-Fi firmware update available")
        : "Wi-Fi firmware updated",
      sub: wifiFirmware.installing ? fw?.install.message
        : fw?.reboot_pending ? "Restart to finish the update and restore normal Wi-Fi performance."
        : wifiFirmware.show ? "Review the optional firmware update for this Pi."
        : "You can restore the previous firmware if needed.",
      action: (
        <div className="flex shrink-0 gap-2">
          <button onClick={() => setWifiFwOpen(true)} className="action-chip action-chip--accent">
            {wifiFirmware.installing ? "View progress" : fw?.reboot_pending ? "Finish" : wifiFirmware.show ? "Review" : "Details"}
            <ChevronRight className="h-3.5 w-3.5" />
          </button>
          {wifiFirmware.offerRevert && !fw?.reboot_pending && (
            <button onClick={wifiFirmware.dismissRevert} className="action-chip">Dismiss</button>
          )}
        </div>
      ),
    })
  }
  if (updateInfo.available) {
    banners.push({
      id: "update",
      kind: "update",
      icon: <Download className="h-4 w-4" />,
      title: `Update Available${
        updateInfo.latestVersion ? `: ${updateInfo.latestVersion}` : ""
      }`,
      sub: "Go to Settings to install",
      action: (
        <Link
          to="/settings?tab=Device"
          className="action-chip action-chip--accent shrink-0"
        >
          Install <ChevronRight className="h-3.5 w-3.5" />
        </Link>
      ),
    })
  }

  return (
    <div className="space-y-3">
      <div>
        <h1 className="text-2xl font-bold text-slate-100">Dashboard</h1>
        <p className="mt-0.5 text-sm text-slate-500">System overview and status</p>
      </div>

      {error && <p role="status" className="text-xs text-amber-300">Reconnecting · showing the last update</p>}
      <BannerStack banners={banners} />
      {wifiFwOpen && wifiFirmware.status && (
        <WifiFirmwareModal status={wifiFirmware.status} onClose={() => setWifiFwOpen(false)} onRefresh={wifiFirmware.refresh} />
      )}

      <div className="tile-grid">
        <SystemTile
          status={status}
          uptime={uptime}
          useFahrenheit={systemUseFahrenheit}
        />
        <NetworkTile status={status} />
        <StorageTile
          status={status}
          breakdown={storageBreakdown}
        />
        <ActivityTile
          status={archiveStatus}
          fresh={archiveFresh}
          cancelPending={cancelPending}
          cancelError={cancelError}
          onCancel={cancelArchive}
        />
      </div>
    </div>
  )
}

function SystemTile({
  status,
  uptime,
  useFahrenheit,
}: {
  status: PiStatus
  uptime: number
  useFahrenheit: boolean
}) {
  const cpuTemp = parseInt(status.cpu_temp)
  return (
    <StatusTile
      icon={<Activity className="h-4 w-4" />}
      halo={getTempHalo(cpuTemp)}
      title="System"
    >
      <Row
        icon={<Clock className="h-3.5 w-3.5" />}
        label="Uptime"
        value={formatUptime(uptime)}
      />
      <Row
        icon={<Thermometer className="h-3.5 w-3.5" />}
        label="CPU"
        value={cpuTemp > 0 ? formatTemp(cpuTemp, useFahrenheit) : "N/A"}
        valueColor={cpuTemp > 0 ? getTempColor(cpuTemp) : undefined}
      />
      {status.fan_speed && (
        <Row
          icon={<Wind className="h-3.5 w-3.5" />}
          label="Fan"
          value={`${status.fan_speed} RPM`}
        />
      )}
      {typeof status.supply_voltage === "number" && Number.isFinite(status.supply_voltage) && status.supply_voltage > 0 && (
        <Row icon={<Zap className="h-3.5 w-3.5" />} label="5V supply" value={`${status.supply_voltage.toFixed(2)} V`} />
      )}
      {/* Host connectivity requires UDC configured, not merely configfs binding. */}
      <Row
        icon={<HardDrive className="h-3.5 w-3.5" />}
        label="USB Drives"
        {...(() => {
          const drivesState =
            status.drives_active !== "yes"
              ? "disconnected"
              : status.udc_state && status.udc_state !== "configured"
                ? "no-link"
                : "connected"
          const pill = {
            disconnected: { value: "Disconnected", valueColor: "#fbbf24" },
            "no-link": { value: "No host link", valueColor: "#f87171" },
            connected: { value: "Connected", valueColor: "oklch(0.78 0.14 240)" },
          } as const
          return pill[drivesState]
        })()}
      />
    </StatusTile>
  )
}

function NetworkTile({ status }: { status: PiStatus }) {
  const haveWifi = !!status.wifi_ssid
  const haveEth = !!status.ether_speed && status.ether_speed !== "Unknown!"
  const halo: Halo = haveWifi || haveEth ? "accent" : "red"

  return (
    <StatusTile
      icon={haveWifi || haveEth ? <Wifi className="h-4 w-4" /> : <WifiOff className="h-4 w-4" />}
      halo={halo}
      title="Network"
    >
      {haveWifi ? (
        <>
          <div className="tile-row">
            <span className="inline-flex text-slate-500">
              <Wifi className="h-3.5 w-3.5" />
            </span>
            <span className="text-xs font-medium text-slate-200">
              {status.wifi_ssid}
            </span>
            <span className="ml-auto inline-flex items-center gap-1.5 text-[10px] text-slate-500">
              {status.wifi_signal_dbm != null && (
                <span className="text-slate-400">{status.wifi_signal_dbm} dBm</span>
              )}
              <WifiBars bars={getWifiStrengthBars(status.wifi_strength)} />
            </span>
          </div>
          <div className="tile-row pl-5" style={{ minHeight: 18 }}>
            <span className="text-[10px] text-slate-500">{status.wifi_ip || "No IP"}</span>
            {(status.wifi_rx_bps !== undefined || status.wifi_tx_bps !== undefined) && (
              <>
                <span className="ml-auto text-[10px] text-emerald-400">
                  ↓ {formatThroughput(status.wifi_rx_bps ?? 0)}
                </span>
                <span className="text-[10px] text-slate-500">·</span>
                <span className="text-[10px] text-sky-400">
                  ↑ {formatThroughput(status.wifi_tx_bps ?? 0)}
                </span>
              </>
            )}
          </div>
        </>
      ) : (
        <Row
          icon={<WifiOff className="h-3.5 w-3.5" />}
          label="WiFi"
          sub="Not connected"
        />
      )}

      {haveEth ? (
        <>
          <div className="tile-row">
            <span className="inline-flex text-slate-500">
              <EthernetPort className="h-3.5 w-3.5" />
            </span>
            <span className="text-xs font-medium text-slate-200">
              {status.ether_speed}
            </span>
            {status.ether_ip && (
              <span className="ml-auto text-[10px] text-slate-500">
                {status.ether_ip}
              </span>
            )}
          </div>
          {(status.ether_rx_bps !== undefined || status.ether_tx_bps !== undefined) && (
            <div className="tile-row pl-5" style={{ minHeight: 18 }}>
              <span className="text-[10px] text-emerald-400">
                ↓ {formatThroughput(status.ether_rx_bps ?? 0)}
              </span>
              <span className="text-[10px] text-slate-500">·</span>
              <span className="text-[10px] text-sky-400">
                ↑ {formatThroughput(status.ether_tx_bps ?? 0)}
              </span>
            </div>
          )}
        </>
      ) : (
        // Preserve the tile row while showing disconnected Ethernet as muted.
        <div className="tile-row">
          <span className="inline-flex text-slate-600">
            <EthernetPort className="h-3.5 w-3.5" />
          </span>
          <span className="text-xs text-slate-600">Ethernet</span>
          <span className="ml-auto text-[10px] text-slate-600">Not connected</span>
        </div>
      )}
    </StatusTile>
  )
}

function StorageTile({
  status,
  breakdown,
}: {
  status: PiStatus
  breakdown: StorageBreakdown | null
}) {
  const totalSpace = Math.max(0, parseInt(status.total_space) || 0)
  const freeSpace = Math.max(0, Math.min(totalSpace, parseInt(status.free_space) || 0))
  const usedSpace = totalSpace - freeSpace
  const health = status.storage_health
  const storageHalo: Halo = health?.state === "fail" ? "red" : health?.state === "warn" ? "amber" : health?.state === "recovering" ? "blue" : "accent"
  const usedPct = totalSpace > 0 ? (usedSpace / totalSpace) * 100 : 0
  const usedPctStr = totalSpace > 0 ? `${Math.round(usedPct)}%` : "0%"
  const snaps = parseInt(status.num_snapshots)

  const segments = breakdown
    ? [
        { label: "Dashcam", size: breakdown.cam_size, color: "#3b82f6" },
        { label: "Snapshots", size: breakdown.snapshots_size, color: "#6366f1" },
      ].filter((s) => s.size > 0)
    : []

  return (
    <StatusTile
      icon={<HardDrive className="h-4 w-4" />}
      halo={storageHalo}
      title="Storage"
    >
      {health && <p className={`text-[11px] ${health.state === "fail" ? "text-red-400" : health.state === "warn" ? "text-amber-300" : "text-slate-400"}`} role="status">{health.message}</p>}
      <div className="flex items-baseline gap-1.5">
        <span className="text-sm font-semibold text-slate-100">
          {totalSpace > 0 ? formatBytes(usedSpace) : "Capacity unavailable"}
        </span>
        {totalSpace > 0 && <span className="text-[11px] text-slate-500">
          / {formatBytes(totalSpace)} · {usedPctStr} used
        </span>}
        {/* Explain high usage because snapshots rotate as space tightens. */}
        <span className="group relative inline-flex items-center self-center">
          <Info
            aria-label="About storage management"
            className="h-3 w-3 cursor-help text-slate-600 transition-colors hover:text-slate-400"
          />
          <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 w-64 rounded-xl border border-white/10 bg-slate-900 p-3 text-[11px] leading-relaxed text-slate-400 opacity-0 shadow-xl transition-opacity group-hover:pointer-events-auto group-hover:opacity-100">
            <span className="absolute bottom-full right-3 block border-4 border-transparent border-b-slate-900" />
            Dash USB automatically manages your storage. Old
            snapshots are deleted when space is needed. High usage can be normal
            while recording. The storage health message reports whether cleanup
            needs attention.
          </span>
        </span>
      </div>
      {breakdown && breakdown.total_space > 0 && segments.length > 0 ? (
        <>
          <div className="seg-bar">
            {segments.map((s) => (
              <div
                key={s.label}
                style={{
                  width: `${Math.max((s.size / breakdown.total_space) * 100, 0.5)}%`,
                  backgroundColor: s.color,
                }}
                title={`${s.label}: ${formatBytes(s.size)}`}
              />
            ))}
          </div>
          <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1">
            {segments.map((s) => (
              <div key={s.label} className="flex items-center gap-1.5 text-[10px]">
                <span
                  className="inline-block h-1.5 w-1.5 rounded-full"
                  style={{ backgroundColor: s.color }}
                />
                <span className="text-slate-400">{s.label}</span>
                <span className="font-medium text-slate-300">
                  {formatBytes(s.size)}
                </span>
              </div>
            ))}
            <div className="flex items-center gap-1.5 text-[10px]">
              <span className="inline-block h-1.5 w-1.5 rounded-full bg-slate-700" />
              <span className="text-slate-400">Free</span>
              <span className="font-medium text-slate-300">
                {formatBytes(breakdown.free_space)}
              </span>
            </div>
          </div>
        </>
      ) : totalSpace > 0 ? (
        <div className="bar">
          <div
            className="bg-gradient-to-r from-blue-500 to-blue-400"
            style={{ width: `${usedPct}%` }}
          />
        </div>
      ) : null}
      <TileDivider />
      <Row
        icon={<Camera className="h-3.5 w-3.5" />}
        label={`${snaps.toLocaleString()} snapshots`}
        sub={
          snaps > 0
            ? `${new Date(
                parseInt(status.snapshot_oldest) * 1000
              ).toLocaleDateString()} → ${new Date(
                parseInt(status.snapshot_newest) * 1000
              ).toLocaleDateString()}`
            : "—"
        }
      />
    </StatusTile>
  )
}

function ActivityTile({
  status,
  fresh,
  cancelPending,
  cancelError,
  onCancel,
}: {
  status: ArchiveStatus | null
  fresh: boolean
  cancelPending: boolean
  cancelError: string
  onCancel: () => void
}) {
  const archiving = status?.phase === "archiving" || !!status?.cycle
  const cancelling = cancelPending || !!status?.cycle?.cancelling
  const total = status?.total ?? 0
  const current = Math.max(0, status?.current ?? 0)
  const pct = total > 0 ? Math.min(100, Math.max(0, current / total * 100)) : 0
  const estimate = status?.eta_seconds
  const seconds = typeof estimate === "number" && Number.isFinite(estimate) && estimate >= 0 ? estimate : null
  const eta = !fresh ? "Waiting for connection…"
    : status?.eta_state === "stalled" ? "Waiting for transfer progress…"
    : status?.eta_state === "estimating" ? "Estimating…"
    : status?.eta_state === "unavailable" ? "Estimate unavailable"
    : seconds == null ? "Estimate unavailable"
    : seconds < 60 ? "Less than a minute remaining"
    : seconds < 3600 ? `About ${Math.ceil(seconds / 60)} min remaining`
    : `About ${(seconds / 3600).toFixed(1)} h remaining`

  return (
    <div className="relative flex flex-col">
      {archiving && (
        <div className="pointer-events-none absolute right-2 top-2 z-10">
          <Pill kind="accent"><LiveDot /> {cancelling ? "cancelling" : "archiving"}</Pill>
        </div>
      )}
      <StatusTile icon={<Zap className="h-4 w-4" />} halo="violet" title="Activity" className="flex-1">
        {archiving ? (
          <>
            <p className="t-xs">{cancelling ? "Stopping this archive safely…" : "Archiving recordings to your configured destination."}</p>
            {total > 0 && (
              <>
                <div className="flex items-center justify-between text-[10px] text-slate-500 t-num">
                  <span>{current.toLocaleString()} / {total.toLocaleString()} files</span><span>{Math.round(pct)}%</span>
                </div>
                <div className="bar" role="progressbar" aria-label="Archive progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)}>
                  <div className="bg-gradient-to-r from-emerald-500 to-emerald-400" style={{ width: `${pct}%` }} />
                </div>
              </>
            )}
            {!cancelling && <p className="text-[11px] text-slate-400">{eta}</p>}
            <button className="action-chip self-start disabled:cursor-not-allowed disabled:opacity-50" disabled={cancelling || !fresh || !status?.cycle?.id} onClick={onCancel}>
              {cancelling ? "Cancelling…" : "Cancel archive"}
            </button>
            <p className="text-[10px] text-slate-500">
              {!status?.cycle?.id ? "Cancellation becomes available when this cycle is ready."
                : "Cancels this cycle. Remaining footage is kept, and future archives still run automatically."}
            </p>
            {cancelError && <p role="alert" className="text-xs text-red-400">{cancelError}</p>}
          </>
        ) : (
          <p className="t-xs">{!status ? "Checking archive status…" : status.phase === "cancelled" ? "Archive cancelled. The next cycle starts automatically when ready." : "Idle. Snapshots are captured continuously; archiving starts automatically when the archive destination is reachable."}</p>
        )}
      </StatusTile>
    </div>
  )
}
