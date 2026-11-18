import assert from 'node:assert/strict'
import test from 'node:test'
import { archiveCategoryChange, archivedCategories, selectedVehicle, storageError, vehicleChange, type VehicleList } from './vehicles.ts'

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

const rivian = {
  ...vehicles.profiles[1],
  categories: [
    { category: 'Continuous', label: 'Recordings' }, { category: 'Incidents', label: 'Incidents' }, { category: 'Saved', label: 'Saved Clips' },
  ],
}

test('every category archives until the owner picks a subset', () => {
  assert.deepEqual(archivedCategories({}, rivian), ['Continuous', 'Incidents', 'Saved'])
  assert.deepEqual(archivedCategories({ ARCHIVE_CATEGORIES: '' }, rivian), ['Continuous', 'Incidents', 'Saved'])
  // Listed in the vehicle's order; names it does not have are ignored.
  assert.deepEqual(archivedCategories({ ARCHIVE_CATEGORIES: 'Saved, Continuous,Launch' }, rivian), ['Continuous', 'Saved'])
  // Older firmware sends no category list, and no vehicle means nothing to choose.
  assert.deepEqual(archivedCategories({ ARCHIVE_CATEGORIES: 'Saved' }, vehicles.profiles[0]), [])
  assert.deepEqual(archivedCategories({}, null), [])
})

test('ticking categories writes a list only for a strict subset', () => {
  assert.deepEqual(archiveCategoryChange('Incidents', false, {}, rivian), { ARCHIVE_CATEGORIES: 'Continuous,Saved' })
  assert.deepEqual(archiveCategoryChange('Saved', false, { ARCHIVE_CATEGORIES: 'Continuous,Saved' }, rivian), { ARCHIVE_CATEGORIES: 'Continuous' })
  // Back to everything: no list, so categories added by a later profile archive too.
  assert.deepEqual(archiveCategoryChange('Incidents', true, { ARCHIVE_CATEGORIES: 'Continuous,Saved' }, rivian), { ARCHIVE_CATEGORIES: '' })
  // Unticking the last one turns archiving off instead of saving an empty list.
  assert.deepEqual(archiveCategoryChange('Continuous', false, { ARCHIVE_CATEGORIES: 'Continuous' }, rivian), { ARCHIVE_RECORDINGS: 'false', ARCHIVE_CATEGORIES: '' })
})

test('picking another vehicle drops a category selection made for the old one', () => {
  assert.deepEqual(
    vehicleChange('gm_surroundvision', { VEHICLE_PROFILE: 'rivian_roadcam', CAM_SIZE: '200G', ARCHIVE_CATEGORIES: 'Incidents' }, vehicles),
    { VEHICLE_PROFILE: 'gm_surroundvision', ARCHIVE_CATEGORIES: '' },
  )
})
