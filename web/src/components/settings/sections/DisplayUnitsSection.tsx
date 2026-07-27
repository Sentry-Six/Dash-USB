import { Thermometer } from "lucide-react"
import { PrefCard } from "@/components/settings/PrefCard"
import { useUnits } from "@/lib/units"
import { cn } from "@/lib/utils"

export function DisplayUnitsSection() {
  // One key (TEMPERATURE_UNIT) governs every CPU-temperature readout —
  // dashboard tile, alert notifications, health checks, and logs.
  const { isMetric, setMetric } = useUnits()

  return (
    <PrefCard
      icon={<Thermometer className="h-3.5 w-3.5" />}
      halo="violet"
      title="Display & Units"
      badge={
        // Connected pill, borrowing the Keep Awake SegPicker's green palette
        // (border-blue-500/40 bg-blue-500/10 text-blue-400 — hue-150 green).
        <span
          role="tablist"
          aria-label="Units"
          className="flex items-center rounded-full border border-white/10 bg-slate-800/80 p-0.5"
        >
          <button
            type="button"
            role="tab"
            aria-selected={isMetric}
            onClick={() => setMetric(true)}
            className={cn(
              "rounded-full border px-3 py-0.5 text-xs font-medium transition-colors",
              isMetric
                ? "border-blue-500/40 bg-blue-500/10 text-blue-400"
                : "border-transparent text-slate-400 hover:bg-white/[0.05]",
            )}
          >
            Metric
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={!isMetric}
            onClick={() => setMetric(false)}
            className={cn(
              "rounded-full border px-3 py-0.5 text-xs font-medium transition-colors",
              !isMetric
                ? "border-blue-500/40 bg-blue-500/10 text-blue-400"
                : "border-transparent text-slate-400 hover:bg-white/[0.05]",
            )}
          >
            Imperial
          </button>
        </span>
      }
    >
      <p className="text-xs text-slate-500">
        Metric shows °C, Imperial shows °F — applied to the Pi CPU temperature
        everywhere: the System tile, temperature alerts, health checks, and logs.
      </p>
    </PrefCard>
  )
}
