import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement, StrictMode } from 'react'
import { Window } from 'happy-dom'
import Viewer from './Viewer.tsx'

async function mount(run: (container: HTMLElement) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/viewer', width: 1280, height: 900 })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true,
    requestAnimationFrame: win.requestAnimationFrame.bind(win), cancelAnimationFrame: win.cancelAnimationFrame.bind(win) }
  const originals = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try { await act(async () => root.render(createElement(StrictMode, {}, createElement(Viewer)))); await run(container as unknown as HTMLElement) }
  finally { await act(async () => root.unmount()); for (const [key, descriptor] of originals) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }; win.close() }
}

const profile = {
  id: 'test', display_name: 'Test', segment_seconds: 300,
  cameras: [{ id: 'FRONT', label: 'Front' }], grid: [['FRONT']],
  filename_regex: '^(?P<camera>FRONT)_(?P<y>\\d{4})_(?P<mo>\\d{2})_(?P<d>\\d{2})_T_(?P<h>\\d{2})_(?P<mi>\\d{2})_(?P<s>\\d{2})\\.mp4$',
}
const clip = (date: string, minutes: number[]) => ({
  date, path: `/mutable/Recordings/Continuous/${date}`,
  files: minutes.map(m => `FRONT_2026_07_17_T_19_${String(m).padStart(2, '0')}_00.mp4`),
})

test('Viewer regroups a clip chosen before the profile loads, then follows clip switches and deletion', async () => {
  const oldFetch = globalThis.fetch
  const profileReads: ((response: Response) => void)[] = []
  const deletes: string[] = []
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (url === '/api/profile') return new Promise(resolve => profileReads.push(resolve))
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'Continuous', clips: [clip('first', [30, 35]), clip('second', [40])], hasMore: false }])
    if (init?.method === 'DELETE') { deletes.push(url); return new Response(null, { status: 204 }) }
    return Response.json({})
  }
  try { await mount(async container => {
    const button = (text: string) => [...container.querySelectorAll('button')].find(b => b.textContent?.includes(text))!
    await act(async () => button('2 files').click())
    assert.ok(container.textContent?.includes('No video files found'))
    await act(async () => { for (const resolve of profileReads) resolve(Response.json(profile)) })
    assert.ok(container.textContent?.includes('0:00 / 10:00'))
    await act(async () => button('1 files').click())
    assert.ok(container.textContent?.includes('0:00 / 5:00'))
    await act(async () => (container.querySelectorAll('button[title="Delete clip"]')[1] as HTMLButtonElement).click())
    await act(async () => button('Yes').click())
    assert.equal(deletes.length, 1)
    assert.ok(container.textContent?.includes('Select a clip to begin playback'))
    assert.ok(!container.textContent?.includes('1 files'))
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer aborts the StrictMode replayed clip read and keeps the live response', async () => {
  const oldFetch = globalThis.fetch
  const clipReads: { signal: AbortSignal; resolve: (response: Response) => void }[] = []
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (url.startsWith('/api/clips?')) return new Promise(resolve => clipReads.push({ signal: init?.signal as AbortSignal, resolve }))
    return Response.json(profile)
  }
  try { await mount(async container => {
    assert.equal(clipReads.length, 2)
    assert.equal(clipReads[0].signal.aborted, true)
    assert.equal(clipReads[1].signal.aborted, false)
    await act(async () => clipReads[1].resolve(Response.json([{ name: 'Continuous', clips: [clip('live', [30])] }])))
    await act(async () => clipReads[0].resolve(Response.json([{ name: 'Continuous', clips: [clip('stale', [30, 35])] }])))
    assert.ok(container.textContent?.includes('1 files'))
    assert.ok(!container.textContent?.includes('2 files'))
  }) } finally { globalThis.fetch = oldFetch }
})
