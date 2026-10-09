import { useSyncExternalStore } from "react"

// Shared temperature-unit preference backed by setup configuration.
type UnitState = {
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

// The first subscriber refreshes out-of-band edits; mounted consumers share state.
function subscribe(cb: () => void): () => void {
  const wasEmpty = listeners.size === 0
  listeners.add(cb)
  if (wasEmpty) void load()
  return () => {
    listeners.delete(cb)
  }
}

// Optimistically update, then revert if the full-config save fails.
async function writeKeys(updates: Record<string, string>, optimistic: Partial<UnitState>) {
  const prev = state
  set(optimistic)
  try {
    const res = await fetch("/api/setup/config")
    // Never replace the full config from an empty failed-read baseline.
    if (!res.ok) throw new Error("could not read current config")
    const cfg = await res.json()
    if (!cfg || typeof cfg !== "object" || Array.isArray(cfg)) {
      throw new Error("unexpected config shape")
    }
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
    isMetric: !s.tempF,
    setMetric: (metric: boolean) =>
      writeKeys({ TEMPERATURE_UNIT: metric ? "C" : "F" }, { tempF: !metric }),
    setTempF: (f: boolean) =>
      writeKeys({ TEMPERATURE_UNIT: f ? "F" : "C" }, { tempF: f }),
  }
}
