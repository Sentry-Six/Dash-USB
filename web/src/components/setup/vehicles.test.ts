import assert from 'node:assert/strict'
import test from 'node:test'
import { selectedVehicle, storageError, vehicleChange, type VehicleList } from './vehicles.ts'

const vehicles: VehicleList = {
  active: 'gm_surroundvision',
  profiles: [
    { id: 'gm_surroundvision', display_name: 'GM Surround Vision', brand: 'GM', experimental: false, default_size: '64G', min_size: '64G', rolling_window_minutes: 120, approx_gb_per_hour: 5.3 },
    { id: 'rivian_roadcam', display_name: 'Rivian Road Cam', brand: 'Rivian', experimental: true, default_size: '128G', min_size: '128G', rolling_window_minutes: 0, approx_gb_per_hour: null },
  ],
}

test('the form selects the configured vehicle, else the one the device is running', () => {
  assert.equal(selectedVehicle({}, null), null)
  assert.equal(selectedVehicle({}, vehicles)?.id, 'gm_surroundvision')
  assert.equal(selectedVehicle({}, { ...vehicles, active: 'rivian_roadcam' })?.id, 'rivian_roadcam')
  assert.equal(selectedVehicle({ VEHICLE_PROFILE: 'rivian_roadcam' }, vehicles)?.id, 'rivian_roadcam')
  // A profile this firmware no longer ships falls back to the first one.
  assert.equal(selectedVehicle({ VEHICLE_PROFILE: 'gone' }, vehicles)?.id, 'gm_surroundvision')
})

test('picking a vehicle moves the drive size only when the car needs it to', () => {
  // Too small for the new car, and also the old car's default.
  assert.deepEqual(vehicleChange('rivian_roadcam', { CAM_SIZE: '64' }, vehicles), { VEHICLE_PROFILE: 'rivian_roadcam', CAM_SIZE: '128G' })
  // Back again: the size was the previous default, so it follows the new one.
  assert.deepEqual(vehicleChange('gm_surroundvision', { VEHICLE_PROFILE: 'rivian_roadcam', CAM_SIZE: '128G' }, vehicles), { VEHICLE_PROFILE: 'gm_surroundvision', CAM_SIZE: '64G' })
  // A size the user chose that the new car accepts is left alone.
  assert.deepEqual(vehicleChange('rivian_roadcam', { CAM_SIZE: '200G' }, vehicles), { VEHICLE_PROFILE: 'rivian_roadcam' })
  // Megabyte sizes are never large enough for a car.
  assert.deepEqual(vehicleChange('rivian_roadcam', { CAM_SIZE: '500M' }, vehicles), { VEHICLE_PROFILE: 'rivian_roadcam', CAM_SIZE: '128G' })
  assert.deepEqual(vehicleChange('unknown', { CAM_SIZE: '64' }, vehicles), {})
})

test('the storage step enforces the chosen vehicle minimum', () => {
  assert.equal(storageError({ CAM_SIZE: '0' }, vehicles), 'Dashcam drive size must be greater than 0 GB.')
  assert.equal(storageError({ CAM_SIZE: '64G' }, vehicles), null)
  assert.equal(storageError({ CAM_SIZE: '32G' }, vehicles), 'GM requires a dashcam drive of at least 64 GB.')
  assert.equal(storageError({ CAM_SIZE: '64G', VEHICLE_PROFILE: 'rivian_roadcam' }, vehicles), 'Rivian requires a dashcam drive of at least 128 GB.')
  assert.equal(storageError({ CAM_SIZE: '128G', VEHICLE_PROFILE: 'rivian_roadcam' }, vehicles), null)
  // Before the list loads the GM minimum still applies.
  assert.equal(storageError({ CAM_SIZE: '32G' }, null), 'GM requires a dashcam drive of at least 64 GB.')
})
