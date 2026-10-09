import { Info, Wifi } from "lucide-react"
import type { StepProps } from "../SetupWizard"
import { SecretInput } from "../SecretInput"
import { cn } from "@/lib/utils"

function Field({
  label,
  field,
  type = "text",
  placeholder,
  data,
  onChange,
  hint,
  error,
}: {
  label: string
  field: string
  type?: string
  placeholder?: string
  data: StepProps["data"]
  onChange: StepProps["onChange"]
  hint?: string
  error?: boolean
}) {
  const inputCls = cn(
    "w-full rounded-lg border bg-white/5 px-3 py-2 text-sm text-slate-100 placeholder-slate-600 outline-none transition focus:ring-1",
    error
      ? "border-red-500/50 focus:border-red-500/50 focus:ring-red-500/25"
      : "border-white/10 focus:border-blue-500/50 focus:ring-blue-500/25"
  )
  return (
    <div>
      <label className="mb-1 block text-sm font-medium text-slate-300">
        {label}
      </label>
      {type === "password" ? (
        <SecretInput
          value={data[field] ?? ""}
          onChange={(v) => onChange(field, v)}
          placeholder={placeholder}
          className={cn(inputCls, "pr-8")}
        />
      ) : (
        <input
          type={type}
          value={data[field] ?? ""}
          onChange={(e) => onChange(field, e.target.value)}
          placeholder={placeholder}
          className={inputCls}
        />
      )}
      {hint && <p className="mt-1 text-xs text-slate-600">{hint}</p>}
    </div>
  )
}

export function NetworkStep({ data, onChange }: StepProps) {

  return (
    <div className="space-y-6">
      <div className="rounded-lg border border-blue-500/20 bg-blue-500/5 p-4">
        <div className="flex items-start gap-3">
          <Info className="mt-0.5 h-5 w-5 shrink-0 text-blue-400" />
          <div>
            <p className="text-sm font-medium text-slate-200">
              WiFi is configured during SD card imaging
            </p>
            <p className="mt-1 text-xs leading-relaxed text-slate-400">
              Set your WiFi network name, password, and country code in
              <span className="font-medium text-slate-300"> Raspberry Pi Imager </span>
              before flashing your SD card. Dash USB will use that WiFi configuration automatically.
            </p>
            <p className="mt-2 text-xs text-slate-500">
              If you need to change WiFi later, re-flash the SD card with updated settings or
              use <code className="rounded bg-white/5 px-1 py-0.5 text-slate-400">sudo nmcli device wifi connect &quot;SSID&quot; password &quot;PASS&quot;</code> via SSH.
            </p>
          </div>
        </div>
      </div>

      <div>
        <div className="mb-3 flex items-center gap-2">
          <Wifi className="h-4 w-4 text-blue-400" />
          <h3 className="text-sm font-semibold uppercase tracking-wider text-slate-400">
            Hostname
          </h3>
        </div>
        <Field
          label="Device Hostname"
          field="DASHUSB_HOSTNAME"
          placeholder="dashusb"
          data={data}
          onChange={onChange}
          hint="The device will be accessible at hostname.local (e.g. dashusb.local)"
        />
      </div>

    </div>
  )
}
