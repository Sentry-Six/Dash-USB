import { useState, useEffect } from "react"
import { HardDrive, RefreshCw } from "lucide-react"
import type { StepProps } from "../SetupWizard"
import { SizeInput } from "../SizeInput"

interface BlockDevice {
  path: string
  name: string
  size_gb: string
  model: string
}

export function StorageStep({ data, onChange }: StepProps) {
  const [devices, setDevices] = useState<BlockDevice[]>([])
  const [loadingDevices, setLoadingDevices] = useState(false)

  async function fetchDevices() {
    setLoadingDevices(true)
    try {
      const res = await fetch("/api/system/block-devices")
      const data = await res.json()
      setDevices(Array.isArray(data) ? data : [])
    } catch { setDevices([]) }
    setLoadingDevices(false)
  }

  useEffect(() => { fetchDevices() }, [])

  // Calculate dashcam warning — only meaningful for GB values
  const camRaw = data.CAM_SIZE ?? ""
  const camIsGB = !/[mM]$/.test(camRaw)
  const camSize = parseInt(camRaw.replace(/[^0-9]/g, "") || "0")
  const camWarning = camIsGB && camSize >= 100
    ? "Large dashcam sizes leave very little room for snapshots. The car saves ~1 hour of recent footage and rotates it. If there's not enough free space for snapshots, you won't see clips beyond the last hour. We recommend 40-60 GB for dashcam and leaving the rest for snapshots."
    : camIsGB && camSize >= 80
      ? "Consider leaving more space for snapshots. If the dashcam partition is too large, the car's recent clips may not save properly."
      : undefined

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-2">
        <HardDrive className="h-4 w-4 text-blue-400" />
        <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400">
          Drive Sizes
        </h3>
      </div>

      <p className="text-xs text-slate-500">
        Configure the size of each USB drive partition. Choose GB or MB per drive.
        A 256 GB+ high-endurance SD card is recommended (GM records ~5 GB per hour of driving). The remaining space is used for snapshots — your retained footage history.
      </p>

      <div className="grid gap-3">
        <SizeInput
          label="Dashcam Size"
          field="CAM_SIZE"
          data={data}
          onChange={onChange}
          defaultVal="64"
          hint="GM requires a drive of at least 64 GB, so keep this at 64 or higher. Do NOT use your entire card — leave room for snapshots, which hold your archived footage."
          warning={camWarning}
        />
      </div>

      {/* Data Drive */}
      <div>
        <label className="mb-1 block text-sm font-medium text-slate-300">
          External Data Drive
        </label>
        <div className="flex gap-2">
          <select
            value={data.DATA_DRIVE ?? ""}
            onChange={(e) => onChange("DATA_DRIVE", e.target.value)}
            className="flex-1 rounded-lg border border-white/10 bg-slate-900 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-blue-500/50 focus:ring-1 focus:ring-blue-500/25 [&>option]:bg-slate-900 [&>option]:text-slate-100"
          >
            <option value="">None (use SD card)</option>
            {devices.map((d) => (
              <option key={d.path} value={d.path}>{d.name}</option>
            ))}
          </select>
          <button
            type="button"
            onClick={fetchDevices}
            disabled={loadingDevices}
            className="flex items-center gap-1.5 rounded-lg border border-white/10 bg-white/5 px-3 py-2 text-xs font-medium text-slate-300 transition-colors hover:bg-white/10 disabled:opacity-50"
          >
            <RefreshCw className={`h-3.5 w-3.5 ${loadingDevices ? "animate-spin" : ""}`} />
            Refresh
          </button>
        </div>
        <p className="mt-1 text-xs text-slate-600">
          Optional. Use an external USB or NVMe drive instead of the SD card.
          <span className="font-medium text-amber-400"> WARNING: The selected drive will be wiped.</span>
        </p>
      </div>

    </div>
  )
}
