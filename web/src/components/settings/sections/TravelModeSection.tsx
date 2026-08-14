import { useEffect, useState } from "react"
import { Route } from "lucide-react"
import { PrefCard } from "@/components/settings/PrefCard"
import { Toggle } from "@/components/ui/Toggle"

// archiveloop reads this every cycle. Travel Mode uses paced cycles for
// always-on networks instead of waiting for connectivity to disappear.
export function TravelModeSection() {
  const [enabled, setEnabled] = useState(false)
  const [loaded, setLoaded] = useState(false)

  useEffect(() => {
    fetch("/api/setup/config")
      .then((r) => r.json())
      .then((cfg) => {
        const e = cfg?.TRAVEL_MODE_ENABLED
        const raw = typeof e === "string" ? e : e?.active ? e.value : ""
        setEnabled(["true", "yes", "1", "on"].includes(String(raw).toLowerCase()))
      })
      .catch(() => {})
      .finally(() => setLoaded(true))
  }, [])

  // Update optimistically and revert failed saves.
  async function save(next: boolean) {
    const prev = enabled
    setEnabled(next)
    try {
      const res = await fetch("/api/setup/config")
      // Never replace the full config from an empty failed-read baseline.
      if (!res.ok) throw new Error("could not read current config")
      const cfg = await res.json()
      if (!cfg || typeof cfg !== "object" || Array.isArray(cfg)) {
        throw new Error("unexpected config shape")
      }
      cfg.TRAVEL_MODE_ENABLED = { value: next ? "true" : "false", active: true }
      const flat: Record<string, string> = {}
      for (const [k, v] of Object.entries(cfg)) {
        if (typeof v === "string") flat[k] = v
        else {
          const entry = v as { value: string; active: boolean }
          if (entry?.active) flat[k] = entry.value
        }
      }
      const put = await fetch("/api/setup/config", {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(flat),
      })
      if (!put.ok) throw new Error("save failed")
    } catch {
      setEnabled(prev)
    }
  }

  return (
    <PrefCard
      icon={<Route className="h-3.5 w-3.5" />}
      halo="blue"
      title="Travel Mode"
    >
      <Toggle
        checked={enabled}
        onChange={save}
        disabled={!loaded}
        label="Archive over an always-on connection"
        sub="Turn this on if the Pi reaches your archive through the vehicle's hotspot, a tethered phone, or a VPN. Archiving then runs on a timer and the drive stays connected to the car. Leave it off if you archive over home WiFi when parked."
      />
    </PrefCard>
  )
}
