import { useEffect, useState } from "react"
import { Route } from "lucide-react"
import { PrefCard } from "@/components/settings/PrefCard"
import { Toggle } from "@/components/ui/Toggle"

// Writes TRAVEL_MODE_ENABLED into dashusb.conf. archiveloop re-reads the key
// on every cycle, so the change applies without a restart.
//
// Off: archive when the network appears, then wait for it to disappear. That
// matches a Pi that only reaches its archive when parked at home.
// On: archive on a timer instead, and never disconnect the drive from the car.
// Required for an always-on link (vehicle hotspot, tethered phone, VPN), where
// the network never disappears and the normal flow would stop snapshotting
// after its first cycle.
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

  // Optimistic: flip immediately, revert if the save fails, so the switch
  // never shows a value that didn't persist.
  async function save(next: boolean) {
    const prev = enabled
    setEnabled(next)
    try {
      const res = await fetch("/api/setup/config")
      // MUST abort if the read fails. The PUT is a full-config replace and the
      // writer comments out every active key missing from the payload, so
      // sending a one-key map built from `{}` would disable archive
      // credentials and everything else.
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
