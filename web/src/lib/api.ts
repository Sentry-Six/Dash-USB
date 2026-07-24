const API_BASE = "/api"

// Backend API base URL for resolving relative attachment/media URLs.
// The Pi proxies API requests locally, but media assets are served directly
// by the backend. Override via Vite env for staging/dev.
export const BACKEND_BASE_URL = import.meta.env.VITE_SENTRY_API_URL || "https://api.sentry-six.com"

async function request<T>(path: string, options?: RequestInit): Promise<T> {
  const res = await fetch(`${API_BASE}${path}`, {
    headers: {
      "Content-Type": "application/json",
      ...options?.headers,
    },
    ...options,
  })
  if (!res.ok) {
    throw new Error(`API error: ${res.status} ${res.statusText}`)
  }
  return res.json() as Promise<T>
}

export interface PiStatus {
  cpu_temp: string
  num_snapshots: string
  snapshot_oldest: string
  snapshot_newest: string
  total_space: string
  free_space: string
  uptime: string
  drives_active: string
  /**
   * Host-link state from /sys/class/udc ("configured" = the car is
   * actually enumerated and talking). drives_active only reflects the
   * configfs binding — the Pi's intent to present — and stays "yes"
   * through a dead link. Present only on backends ≥ v3.13.4.
   */
  udc_state?: string
  /** Seconds since the car last wrote to cam_disk.bin, -1 when unknown. */
  cam_last_write_secs?: number
  wifi_ssid: string
  wifi_strength: string
  wifi_ip: string
  ether_ip: string
  ether_speed: string
  fan_speed: string
  sbc_model?: string
  /** Negative integer parsed from iwconfig "Signal level=-48 dBm". Present only on backends ≥ v2.7.4. */
  wifi_signal_dbm?: number
  wifi_rx_bps?: number
  wifi_tx_bps?: number
  ether_rx_bps?: number
  ether_tx_bps?: number
}

export interface EventMeta {
  timestamp?: string
  city?: string
  reason?: string
  camera?: string
  latitude?: string
  longitude?: string
}

export interface ClipGroup {
  name: string
  clips: ClipEntry[]
  hasMore?: boolean
}

export interface ClipEntry {
  date: string
  path: string
  files: string[]
  event?: EventMeta
}

export interface StorageBreakdown {
  cam_size: number
  music_size: number
  snapshots_size: number
  total_space: number
  free_space: number
}

/** Live archive progress from /tmp/archive_status.json (via the API);
 *  phase is "idle" when no batch is running. */
export interface ArchiveStatus {
  phase: string
  current?: number
  total?: number
}

export const api = {
  getStatus: () => request<PiStatus>("/status"),
  getStorageBreakdown: () => request<StorageBreakdown>("/status/storage"),
  getArchiveStatus: () => request<ArchiveStatus>("/archive/status"),
}
