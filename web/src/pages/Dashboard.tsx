import { useEffect, useRef, useState } from "react"
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

function getStorageHalo(usedPct: number): Halo {
  if (usedPct > 90) return "red"
  if (usedPct > 75) return "amber"
  return "accent"
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

// Mini 4-bar signal indicator. Filled bars get the tile's accent colour;
// the rest are a muted slate so the gauge reads at a glance.
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

interface ProcessProgress {
  current: number
  total: number
}
interface ProgressSample {
  time: number
  current: number
}
const RATE_WINDOW = 6

function computeETA(
  current: number,
  total: number,
  history: ProgressSample[]
): string | null {
  if (history.length < 2) return null
  const oldest = history[0]
  const newest = history[history.length - 1]
  const elapsed = (newest.time - oldest.time) / 1000
  const done = newest.current - oldest.current
  if (done <= 0 || elapsed < 5) return null
  const rate = done / elapsed
  const remaining = (total - current) / rate
  if (!isFinite(remaining) || remaining <= 0) return null
  if (remaining < 60) return `~${Math.round(remaining)}s`
  if (remaining < 3600) return `~${Math.round(remaining / 60)}m`
  return `~${(remaining / 3600).toFixed(1)}h`
}

export default function Dashboard() {
  const [status, setStatus] = useState<PiStatus | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [uptime, setUptime] = useState(0)
  const [storageBreakdown, setStorageBreakdown] =
    useState<StorageBreakdown | null>(null)
  const [archiveProgress, setArchiveProgress] = useState<ProcessProgress | null>(null)
  // Units come from the shared store — coherent defaults and live-synced with
  // the Settings → Display & Units controls.
  const { systemTempF: systemUseFahrenheit } = useUnits()
  const [rtcWarning, setRtcWarning] = useState<string | null>(null)

  const archiveHistoryRef = useRef<ProgressSample[]>([])
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
        if (d.phase === "archiving" && d.total != null && d.total > 0) {
          setArchiveProgress({ current: d.current ?? 0, total: d.total })
        } else {
          setArchiveProgress(null)
        }
      } catch {
        /* non-critical */
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

    // Pause every poller while the tab is hidden (phone in a pocket, a
    // backgrounded tab) so the dashboard stops hitting the Pi and
    // draining the phone battery for data nobody's looking at.
    const statusInterval = setInterval(() => {
      if (!document.hidden) fetchStatus()
    }, 2000)
    const archiveInterval = setInterval(() => {
      if (!document.hidden) fetchArchiveStatus()
    }, 5000)
    const storageInterval = setInterval(() => {
      if (!document.hidden) fetchStorageBreakdown()
    }, 10000)
    // Local-only counter, but still skip it while hidden so React isn't
    // re-rendering the dashboard once a second for a backgrounded tab.
    const uptimeInterval = setInterval(() => {
      if (!document.hidden) setUptime((p) => p + 1)
    }, 1000)

    // Snap the live tiles back to current the moment the tab is shown
    // again, rather than waiting for the slower intervals.
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

  useEffect(() => {
    if (archiveProgress && archiveProgress.current > 0) {
      const h = archiveHistoryRef.current
      h.push({ time: Date.now(), current: archiveProgress.current })
      if (h.length > RATE_WINDOW) h.shift()
    } else {
      archiveHistoryRef.current = []
    }
  }, [archiveProgress])

  if (error) {
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

  // Build banner stack — priority sorted (warn > update).
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

      <BannerStack banners={banners} />

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
          archiveProgress={archiveProgress}
          // eslint-disable-next-line react-hooks/refs -- ETA history is intentionally a ref (push-only, no re-render needed).
          archiveEta={archiveProgress ? computeETA(archiveProgress.current, archiveProgress.total, archiveHistoryRef.current) : null}
        />
      </div>
    </div>
  )
}

// ─── Tiles ──────────────────────────────────────────────────────────────────

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
      {/* Three-state: "Connected" needs the host link up ("configured"),
          not just the gadget bound in configfs — a bound gadget with a
          dead link is exactly how the car shows an error while the old
          two-state pill stayed green. Label and color derive from ONE
          state value so they can't drift. */}
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
        // Always render an Ethernet row — keeps tile balanced when WiFi is
        // present but ethernet isn't (or vice versa). Muted styling signals
        // disconnected state without taking the tile's halo over.
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
  const totalSpace = parseInt(status.total_space)
  const freeSpace = parseInt(status.free_space)
  const usedSpace = totalSpace - freeSpace
  const usedPct = totalSpace > 0 ? (usedSpace / totalSpace) * 100 : 0
  const usedPctStr = totalSpace > 0 ? `${Math.round(usedPct)}%` : "0%"
  const snaps = parseInt(status.num_snapshots)

  const segments = breakdown
    ? [
        { label: "Dashcam", size: breakdown.cam_size, color: "#3b82f6" },
        { label: "Music", size: breakdown.music_size, color: "#a855f7" },
        { label: "Snapshots", size: breakdown.snapshots_size, color: "#6366f1" },
      ].filter((s) => s.size > 0)
    : []

  return (
    <StatusTile
      icon={<HardDrive className="h-4 w-4" />}
      halo={getStorageHalo(usedPct)}
      title="Storage"
    >
      <div className="flex items-baseline gap-1.5">
        <span className="text-sm font-semibold text-slate-100">
          {formatBytes(usedSpace)}
        </span>
        <span className="text-[11px] text-slate-500">
          / {formatBytes(totalSpace)} · {usedPctStr} used
        </span>
        {/* Reassurance tooltip — high storage usage triggers panic
            for new users ("96% used!"), but Dash USB rotates
            snapshots automatically as space gets tight. CSS-only
            group-hover so we don't need React state for it. */}
        <span className="group relative inline-flex items-center self-center">
          <Info
            aria-label="About storage management"
            className="h-3 w-3 cursor-help text-slate-600 transition-colors hover:text-slate-400"
          />
          <span className="pointer-events-none absolute right-0 top-full z-50 mt-2 w-64 rounded-xl border border-white/10 bg-slate-900 p-3 text-[11px] leading-relaxed text-slate-400 opacity-0 shadow-xl transition-opacity group-hover:pointer-events-auto group-hover:opacity-100">
            <span className="absolute bottom-full right-3 block border-4 border-transparent border-b-slate-900" />
            Dash USB automatically manages your storage. Old
            snapshots are deleted when space is needed — you don't
            need to manually free up space. Low remaining space is
            normal and expected, especially with dashcam footage
            being continuously saved.
          </span>
        </span>
      </div>
      {breakdown && segments.length > 0 ? (
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
      ) : (
        <div className="bar">
          <div
            className="bg-gradient-to-r from-blue-500 to-blue-400"
            style={{ width: `${usedPct}%` }}
          />
        </div>
      )}
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
  archiveProgress,
  archiveEta,
}: {
  archiveProgress: ProcessProgress | null
  archiveEta: string | null
}) {
  const archiving = archiveProgress != null

  return (
    <div className="relative flex flex-col">
      {/* Phase pill — pinned to the card's top-right corner; only
          renders during an actual archive run. */}
      {archiving && (
        <div className="pointer-events-none absolute right-2 top-2 z-10">
          <Pill kind="accent">
            <LiveDot /> archiving
          </Pill>
        </div>
      )}
      <StatusTile
        icon={<Zap className="h-4 w-4" />}
        halo="violet"
        title="Activity"
        className="flex-1"
      >
        {archiveProgress && archiveProgress.total > 0 ? (
          <>
            <p className="t-xs">
              Archiving recordings to your configured destination.
            </p>
            <ProgressBlock
              current={archiveProgress.current}
              total={archiveProgress.total}
              eta={archiveEta}
              color="emerald"
            />
          </>
        ) : (
          <p className="t-xs">
            Idle. Snapshots are captured continuously; archiving starts
            automatically when the archive destination is reachable.
          </p>
        )}
      </StatusTile>
    </div>
  )
}

function ProgressBlock({
  current,
  total,
  eta,
  color,
}: {
  current: number
  total: number
  eta: string | null
  color: "emerald" | "blue"
}) {
  const pct = (current / total) * 100
  const grad =
    color === "emerald"
      ? "bg-gradient-to-r from-emerald-500 to-emerald-400"
      : "bg-gradient-to-r from-blue-500 to-blue-400"
  return (
    <>
      <div className="flex items-center justify-between text-[10px] text-slate-500 t-num">
        <span>
          {current.toLocaleString()} / {total.toLocaleString()}
          {eta && (
            <span
              className={`ml-1.5 ${
                color === "emerald" ? "text-emerald-400/70" : "text-blue-400/70"
              }`}
            >
              {eta}
            </span>
          )}
        </span>
        <span>{Math.round(pct)}%</span>
      </div>
      <div className="bar">
        <div className={grad} style={{ width: `${pct}%` }} />
      </div>
    </>
  )
}
