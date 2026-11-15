/** Vehicle profiles the setup wizard can choose from (`GET /api/profiles`). */
export interface VehicleOption {
  id: string
  display_name: string
  brand: string
  /** Not yet confirmed in a car. */
  experimental: boolean
  /** Drive sizes as the config writes them, e.g. "64G". */
  default_size: string
  min_size: string
  /** 0 = the car only deletes footage when the drive is full. */
  rolling_window_minutes: number
  approx_gb_per_hour: number | null
}

export interface VehicleList {
  /** Profile the device is running now; the choice when the config names none. */
  active: string
  profiles: VehicleOption[]
}

type FormData = Record<string, string>

/** Whole gigabytes in a profile size such as "64G". */
export function sizeGb(size: string): number {
  return parseInt(size.replace(/[^0-9]/g, "") || "0")
}

/** The vehicle the form currently selects, or null before the list loads. */
export function selectedVehicle(data: FormData, vehicles: VehicleList | null | undefined): VehicleOption | null {
  if (!vehicles) return null
  const id = data.VEHICLE_PROFILE?.trim() || vehicles.active
  return vehicles.profiles.find((p) => p.id === id) ?? vehicles.profiles[0] ?? null
}

/** Form changes for picking `id`: the vehicle, plus a drive size the car accepts. */
export function vehicleChange(id: string, data: FormData, vehicles: VehicleList): FormData {
  const next = vehicles.profiles.find((p) => p.id === id)
  if (!next) return {}
  const updates: FormData = { VEHICLE_PROFILE: id }
  const previous = selectedVehicle(data, vehicles)
  const raw = data.CAM_SIZE ?? ""
  if (/[mM]$/.test(raw)) return { ...updates, CAM_SIZE: next.default_size }
  const cam = sizeGb(raw)
  // Follow the new car's default when the size was the old default or is too small.
  if (cam < sizeGb(next.min_size) || (previous && cam === sizeGb(previous.default_size))) {
    updates.CAM_SIZE = next.default_size
  }
  return updates
}

export function storageError(data: FormData, vehicles: VehicleList | null): string | null {
  // Zero disables the image, which is invalid for this setup flow.
  const cam = parseFloat(data.CAM_SIZE ?? "0")
  if (!Number.isFinite(cam) || cam <= 0) {
    return "Dashcam drive size must be greater than 0 GB."
  }
  // The car refuses drives under its minimum; GM's applies until the list loads.
  const vehicle = selectedVehicle(data, vehicles)
  const min = vehicle ? sizeGb(vehicle.min_size) : 64
  if (cam < min) {
    return `${vehicle?.brand ?? "GM"} requires a dashcam drive of at least ${min} GB.`
  }
  return null
}
