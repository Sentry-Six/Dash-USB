import { useState, useEffect, useCallback, useRef } from "react"
import { AlertTriangle, Car, HardDrive, RefreshCw } from "lucide-react"
import type { StepProps } from "../SetupWizard"
import { SizeInput } from "../SizeInput"
import { selectedVehicle, sizeGb, vehicleChange } from "../vehicles"

interface BlockDevice {
  path: string
  name: string
  size_gb: string
  model: string
}

export function StorageStep({ data, onChange, onBatchChange, vehicles }: StepProps) {
  const [devices, setDevices] = useState<BlockDevice[]>([])
  // Starts true for the mount fetch; Refresh sets it before refetching.
  const [loadingDevices, setLoadingDevices] = useState(true)
  const devicesRequest = useRef<AbortController | null>(null)

  const fetchDevices = useCallback(() => {
    devicesRequest.current?.abort()
    const controller = new AbortController()
    devicesRequest.current = controller
    fetch("/api/system/block-devices", { signal: controller.signal })
      .then((res) => res.json())
      .then((data) => { if (!controller.signal.aborted) setDevices(Array.isArray(data) ? data : []) })
      .catch(() => { if (!controller.signal.aborted) setDevices([]) })
      .finally(() => { if (!controller.signal.aborted) setLoadingDevices(false) })
  }, [])

  useEffect(() => {
    fetchDevices()
    return () => devicesRequest.current?.abort()
  }, [fetchDevices])

  // Drive requirements follow the chosen vehicle; GM's apply until the list loads.
  const vehicle = selectedVehicle(data, vehicles)
  const brand = vehicle?.brand ?? "GM"
  const minGb = vehicle ? sizeGb(vehicle.min_size) : 64
  const windowHours = (vehicle?.rolling_window_minutes ?? 120) / 60
  const gbPerHour = vehicle ? vehicle.approx_gb_per_hour : 5

  // The dashcam warning only applies to GB values.
  const camRaw = data.CAM_SIZE ?? ""
  const camIsGB = !/[mM]$/.test(camRaw)
  const camSize = parseInt(camRaw.replace(/[^0-9]/g, "") || "0")
  const camWarning = camIsGB && camSize > 0 && camSize < minGb
    ? `${brand} requires a drive of at least ${minGb} GB — the car will refuse to record onto anything smaller.`
    // A car that only keeps a short window gains nothing from a bigger drive.
    : camIsGB && windowHours > 0 && camSize >= Math.max(100, minGb * 1.5)
      ? `Large dashcam sizes leave very little room for snapshots — and snapshots are your footage history (the car itself only keeps ~${windowHours} hours). ${minGb} GB is all the car needs; leave the rest for snapshots.`
      : undefined

  return (
    <div className="space-y-6">
      {vehicles && vehicle && (
        <div>
          <div className="mb-3 flex items-center gap-2">
            <Car className="h-4 w-4 text-blue-400" />
            <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400">
              Vehicle
            </h3>
          </div>
          <label htmlFor="vehicle-profile" className="mb-1 block text-sm font-medium text-slate-300">
            Dashcam system
          </label>
          <select
            id="vehicle-profile"
            value={vehicle.id}
            onChange={(e) => onBatchChange(vehicleChange(e.target.value, data, vehicles))}
            className="w-full rounded-lg border border-white/10 bg-slate-900 px-3 py-2 text-sm text-slate-100 outline-none transition focus:border-blue-500/50 focus:ring-1 focus:ring-blue-500/25 [&>option]:bg-slate-900 [&>option]:text-slate-100"
          >
            {vehicles.profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display_name}{p.experimental ? " (experimental)" : ""}
              </option>
            ))}
          </select>
          <p className="mt-1 text-xs text-slate-600">
            Sets where Dash USB looks for recordings and the drive the car expects.
          </p>
          {vehicle.experimental && (
            <div className="mt-2 flex items-start gap-2 rounded-lg border border-amber-500/20 bg-amber-500/5 px-3 py-2">
              <AlertTriangle className="mt-0.5 h-3.5 w-3.5 shrink-0 text-amber-400" />
              <p className="text-xs text-amber-300">
                {vehicle.display_name} support is experimental: it has not been confirmed in a car yet, so recording or archiving may not work.
              </p>
            </div>
          )}
        </div>
      )}

      <div className="flex items-center gap-2">
        <HardDrive className="h-4 w-4 text-blue-400" />
        <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400">
          Drive Sizes
        </h3>
      </div>

      <p className="text-xs text-slate-500">
        Configure the size of each USB drive partition. Choose GB or MB per drive.
        A {minGb * 4} GB+ high-endurance SD card is recommended{gbPerHour ? ` (${brand} records ~${Math.round(gbPerHour)} GB per hour of driving)` : ""}. The remaining space is used for snapshots — your retained footage history.
      </p>

      <div className="grid gap-3">
        <SizeInput
          label="Dashcam Size"
          field="CAM_SIZE"
          data={data}
          onChange={onChange}
          defaultVal={String(vehicle ? sizeGb(vehicle.default_size) : 64)}
          hint={`${brand} requires a drive of at least ${minGb} GB, so keep this at ${minGb} or higher. Do NOT use your entire card — leave room for snapshots, which hold your archived footage.`}
          warning={camWarning}
        />
      </div>

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
            onClick={() => { setLoadingDevices(true); fetchDevices() }}
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
