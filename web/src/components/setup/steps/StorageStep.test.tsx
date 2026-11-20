import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, useState } from 'react'
import { Window } from 'happy-dom'
import { StorageStep } from './StorageStep.tsx'
import type { VehicleList } from '../vehicles.ts'

const vehicles: VehicleList = {
  active: 'gm_surroundvision',
  profiles: [
    { id: 'gm_surroundvision', display_name: 'GM Surround Vision', brand: 'GM', experimental: false, default_size: '64G', min_size: '64G', rolling_window_minutes: 120, approx_gb_per_hour: 5.3 },
    { id: 'rivian_roadcam', display_name: 'Rivian Road Cam', brand: 'Rivian', experimental: true, default_size: '128G', min_size: '128G', rolling_window_minutes: 0, approx_gb_per_hour: null },
  ],
}

/** Holds the form like the wizard does, so a pick re-renders the step. */
function Harness({ list, saved }: { list: VehicleList | null; saved: Record<string, string>[] }) {
  const [data, setData] = useState<Record<string, string>>({ CAM_SIZE: '64' })
  saved.push(data)
  return createElement(StorageStep, {
    data, vehicles: list, setupAlreadyFinished: false,
    onChange: (key: string, value: string) => setData(prev => ({ ...prev, [key]: value })),
    onBatchChange: (updates: Record<string, string>) => setData(prev => ({ ...prev, ...updates })),
  })
}

async function mount(list: VehicleList | null, run: (container: HTMLElement, saved: Record<string, string>[]) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/', width: 1280, height: 900 })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  globalThis.fetch = async () => Response.json([])
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  const saved: Record<string, string>[] = []
  try { await act(async () => root.render(createElement(Harness, { list, saved }))); await run(container as unknown as HTMLElement, saved) }
  finally { await act(async () => root.unmount()); globalThis.fetch = oldFetch; for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }; win.close() }
}

test('StorageStep keeps the GM wording until a vehicle list arrives', async () => {
  await mount(null, async container => {
    assert.equal(container.querySelector('#vehicle-profile'), null)
    assert.ok(container.textContent?.includes('A 256 GB+ high-endurance SD card is recommended (GM records ~5 GB per hour of driving).'))
    assert.ok(container.textContent?.includes('GM requires a drive of at least 64 GB, so keep this at 64 or higher.'))
  })
})

test('StorageStep picks a vehicle and follows its drive requirements', async () => {
  await mount(vehicles, async (container, saved) => {
    const select = container.querySelector('#vehicle-profile') as unknown as HTMLSelectElement
    assert.equal(select.value, 'gm_surroundvision')
    assert.deepEqual([...select.options].map(o => o.textContent), ['GM Surround Vision', 'Rivian Road Cam (experimental)'])
    assert.ok(!container.textContent?.includes('has not been confirmed in a car'))

    await act(async () => { select.value = 'rivian_roadcam'; select.dispatchEvent(new window.Event('change', { bubbles: true })) })
    assert.deepEqual(saved.at(-1), { CAM_SIZE: '128G', VEHICLE_PROFILE: 'rivian_roadcam' })
    assert.ok(container.textContent?.includes('Rivian Road Cam support is experimental: it has not been confirmed in a car yet'))
    assert.ok(container.textContent?.includes('A 512 GB+ high-endurance SD card is recommended. The remaining space'))
    assert.ok(container.textContent?.includes('Rivian requires a drive of at least 128 GB, so keep this at 128 or higher.'))
    // No rolling window, so a larger drive draws no "leave room" warning.
    assert.ok(!container.textContent?.includes('the car itself only keeps'))
  })
})
