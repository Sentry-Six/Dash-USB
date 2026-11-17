import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, useState } from 'react'
import { Window } from 'happy-dom'
import { ArchiveStep } from './ArchiveStep.tsx'
import type { VehicleList } from '../vehicles.ts'

const base = { experimental: false, default_size: '64G', min_size: '64G', approx_gb_per_hour: null }
const vehicles: VehicleList = {
  active: 'gm_surroundvision',
  profiles: [
    { ...base, id: 'gm_surroundvision', display_name: 'GM Surround Vision', brand: 'GM', rolling_window_minutes: 120, categories: [{ category: 'Continuous', label: 'Recordings' }] },
    {
      ...base, id: 'rivian_roadcam', display_name: 'Rivian Road Cam', brand: 'Rivian', rolling_window_minutes: 0,
      categories: [{ category: 'Continuous', label: 'Recordings' }, { category: 'Incidents', label: 'Incidents' }, { category: 'Saved', label: 'Saved Clips' }],
    },
  ],
}

/** Holds the form like the wizard does, so a tick re-renders the step. */
function Harness({ initial, saved }: { initial: Record<string, string>; saved: Record<string, string>[] }) {
  const [data, setData] = useState(initial)
  saved.push(data)
  return createElement(ArchiveStep, {
    data, vehicles, setupAlreadyFinished: false,
    onChange: (key: string, value: string) => setData(prev => ({ ...prev, [key]: value })),
    onBatchChange: (updates: Record<string, string>) => setData(prev => ({ ...prev, ...updates })),
  })
}

async function mount(initial: Record<string, string>, run: (container: HTMLElement, saved: Record<string, string>[]) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/', width: 1280, height: 900 })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const oldFetch = globalThis.fetch
  globalThis.fetch = async () => Response.json({})
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  const saved: Record<string, string>[] = []
  try { await act(async () => root.render(createElement(Harness, { initial, saved }))); await run(container as unknown as HTMLElement, saved) }
  finally { await act(async () => root.unmount()); globalThis.fetch = oldFetch; for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }; win.close() }
}

const boxes = (container: HTMLElement) =>
  Object.fromEntries([...container.querySelectorAll('label')]
    .filter(label => label.querySelector('input[type="checkbox"]'))
    .map(label => [label.textContent!.trim(), label.querySelector('input') as HTMLInputElement]))

test('ArchiveStep shows no category list for a vehicle with one category', async () => {
  await mount({ ARCHIVE_SYSTEM: 'cifs' }, async container => {
    assert.deepEqual(Object.keys(boxes(container)), ['Recordings (everything the car saves)'])
    assert.ok(container.textContent?.includes('The car keeps only a rolling window of footage on the drive; archiving is what preserves it. Turn this off only if local snapshots are all you want.'))
  })
})

test('ArchiveStep lets a multi-folder vehicle choose which categories archive', async () => {
  await mount({ ARCHIVE_SYSTEM: 'cifs', VEHICLE_PROFILE: 'rivian_roadcam' }, async (container, saved) => {
    assert.deepEqual(Object.keys(boxes(container)), ['Footage the car saves', 'Recordings', 'Incidents', 'Saved Clips'])
    assert.ok(['Recordings', 'Incidents', 'Saved Clips'].every(name => boxes(container)[name].checked))
    assert.ok(container.textContent?.includes('The car deletes its oldest recordings once the drive fills'))

    await act(async () => boxes(container)['Incidents'].click())
    assert.equal(saved.at(-1)!.ARCHIVE_CATEGORIES, 'Continuous,Saved')
    assert.equal(boxes(container)['Incidents'].checked, false)
    assert.equal(boxes(container)['Saved Clips'].checked, true)

    await act(async () => boxes(container)['Incidents'].click())
    assert.equal(saved.at(-1)!.ARCHIVE_CATEGORIES, '')

    // Unticking every category turns archiving off and hides the list.
    for (const name of ['Recordings', 'Incidents', 'Saved Clips']) await act(async () => boxes(container)[name].click())
    assert.equal(saved.at(-1)!.ARCHIVE_RECORDINGS, 'false')
    assert.equal(saved.at(-1)!.ARCHIVE_CATEGORIES, '')
    assert.deepEqual(Object.keys(boxes(container)), ['Footage the car saves'])
    assert.equal(boxes(container)['Footage the car saves'].checked, false)
  })
})

test('ArchiveStep hides the archive choices when nothing is archived', async () => {
  await mount({ ARCHIVE_SYSTEM: 'none', VEHICLE_PROFILE: 'rivian_roadcam' }, async container => {
    assert.deepEqual(Object.keys(boxes(container)), [])
  })
})
