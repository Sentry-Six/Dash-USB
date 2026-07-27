import { useSyncExternalStore } from "react"

// Shared unit preference, backed by /api/setup/config. One key —
// TEMPERATURE_UNIT — governs every Pi CPU temperature the product
// shows: the dashboard System tile, temperature-alert notifications,
// health checks, and log entries. (There is exactly one temperature
// source on GM; the old SYSTEM_TEMPERATURE_UNIT split was Tesla-era
// residue and is migrated away at daemon startup.)
export type UnitState = {
  tempF: boolean // TEMPERATURE_UNIT === "F"
  loaded: boolean
}

// Default is metric (°C).
let state: UnitState = {
  tempF: false,
  loaded: false,
}
const listeners = new Set<() => void>()
let loading = false

function snapshot(): UnitState {
  return state
}

function set(patch: Partial<UnitState>) {
  state = { ...state, ...patch }
  for (const l of listeners) l()
}

function readActive(entry: unknown): string | null {
  if (entry == null) return null
  if (typeof entry === "string") return entry
  const e = entry as { value: string; active: boolean }
  return e.active ? e.value : null
}

async function load() {
  if (loading) return
  loading = true
  try {
    const res = await fetch("/api/setup/config")
    const cfg = res.ok ? await res.json() : {}
    const t = readActive(cfg.TEMPERATURE_UNIT)
    set({
      tempF: t != null ? t === "F" : state.tempF,
      loaded: true,
    })
  } catch {
    set({ loaded: true })
  } finally {
    loading = false
  }
}

// Refetch each time the first consumer (re)mounts so navigating back to
// Settings picks up out-of-band edits (raw-config editor, setup wizard),
// while staying live-synced between mounted consumers in between.
function subscribe(cb: () => void): () => void {
  const wasEmpty = listeners.size === 0
  listeners.add(cb)
  if (wasEmpty) void load()
  return () => {
    listeners.delete(cb)
  }
}

// Read-modify-write the whole config with `updates` applied, then reflect
// them locally. Optimistic: state flips immediately, but reverts to the
// prior snapshot if the save fails so the UI never shows a value that
// didn't persist.
async function writeKeys(updates: Record<string, string>, optimistic: Partial<UnitState>) {
  const prev = state
  set(optimistic)
  try {
    const res = await fetch("/api/setup/config")
    const cfg = res.ok ? await res.json() : {}
    for (const [k, v] of Object.entries(updates)) cfg[k] = { value: v, active: true }
    const flat: Record<string, string> = {}
    for (const [k, v] of Object.entries(cfg)) {
      if (typeof v === "string") {
        flat[k] = v
      } else {
        const e = v as { value: string; active: boolean }
        if (e?.active) flat[k] = e.value
      }
    }
    const put = await fetch("/api/setup/config", {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(flat),
    })
    if (!put.ok) throw new Error("config save failed")
  } catch {
    set(prev)
  }
}

export function useUnits() {
  const s = useSyncExternalStore(subscribe, snapshot, snapshot)
  return {
    ...s,
    // Metric = Celsius; Imperial = Fahrenheit.
    isMetric: !s.tempF,
    setMetric: (metric: boolean) =>
      writeKeys({ TEMPERATURE_UNIT: metric ? "C" : "F" }, { tempF: !metric }),
    setTempF: (f: boolean) =>
      writeKeys({ TEMPERATURE_UNIT: f ? "F" : "C" }, { tempF: f }),
  }
}
