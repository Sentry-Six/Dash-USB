import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
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

test('Viewer groups session-folder clips with two-digit years and shows cameras the profile does not declare', async () => {
  const oldFetch = globalThis.fetch
  const sessionProfile = {
    id: 'session', display_name: 'Session', segment_seconds: 900, cameras: [], grid: [],
    filename_regex: String.raw`^(?P<mo>\d{2})_(?P<d>\d{2})_(?P<y>\d{2})_(?P<h>\d{2})(?P<mi>\d{2})(?P<s>\d{2})__(?P<camera>[^/]+?)\.(?:mp4|MP4)$`,
  }
  const day = {
    date: '2026-09-25', path: '/mutable/Recordings/Continuous/2026-09-25',
    files: ['09_25_26_063343__front.mp4', '09_25_26_063343__rear.mp4', '09_25_26_071500__front.mp4'],
  }
  globalThis.fetch = async (input) => {
    const url = String(input)
    if (url === '/api/profile') return Response.json(sessionProfile)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'Continuous', clips: [day], hasMore: false }])
    return Response.json({})
  }
  try { await mount(async container => {
    const button = (text: string) => [...container.querySelectorAll('button')].find(b => b.textContent?.includes(text))!
    await act(async () => button('3 files').click())
    // Two sessions of 15 minutes each, not three unrelated files.
    assert.ok(container.textContent?.includes('0:00 / 30:00'))
    const grid = container.querySelector('div.grid[style]') as HTMLElement
    assert.equal(grid.style.gridTemplateColumns, 'repeat(2, minmax(0, 1fr))')
    // The first camera found is primary and streams; the rest wait for a click.
    assert.deepEqual([...grid.children].map(cell => cell.textContent?.trim()), ['front', 'Click to streamrear'])
    assert.equal(grid.querySelectorAll('video').length, 1)
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer lays out a real Rivian clip with the shipped profile pattern', async () => {
  const oldFetch = globalThis.fetch
  // The pattern must work in JavaScript as well as in the Rust regex engine.
  const toml = readFileSync(new URL('../../../profiles/rivian_roadcam.toml', import.meta.url), 'utf8')
  const rivian = {
    id: 'rivian_roadcam', display_name: 'Rivian Road Cam', segment_seconds: 900,
    filename_regex: toml.match(/^filename_regex = '(.*)'\r?$/m)![1],
    cameras: [
      { id: 'frontCenter', label: 'Front' }, { id: 'sideLeft', label: 'Left' }, { id: 'sideRight', label: 'Right' },
      { id: 'rearCenter', label: 'Rear' }, { id: 'gearGuard', label: 'Gear Guard', optional: true },
    ],
    grid: [['sideLeft', 'frontCenter', 'sideRight'], ['', 'rearCenter', '']],
  }
  // File names from an owner's clip, linked under their session folder, plus the next segment.
  const cameras = ['sideRight', 'frontCenter', 'gearGuard', 'rearCenter', 'sideLeft']
  const day = {
    date: '2026-09-05', path: '/mutable/Recordings/Continuous/2026-09-05',
    files: [
      ...cameras.map(c => `09_05_26_173818__09_05_26_174959_video_${c}.mp4`),
      '09_05_26_173818__09_05_26_180459_video_frontCenter.mp4',
    ],
  }
  globalThis.fetch = async (input) => {
    const url = String(input)
    if (url === '/api/profile') return Response.json(rivian)
    if (url.startsWith('/api/clips?')) return Response.json([{ name: 'Continuous', clips: [day], hasMore: false }])
    return Response.json({})
  }
  try { await mount(async container => {
    const button = (text: string) => [...container.querySelectorAll('button')].find(b => b.textContent?.includes(text))!
    await act(async () => button('6 files').click())
    // Two segments: the five cameras of one moment group together.
    assert.ok(container.textContent?.includes('0:00 / 30:00'))
    const grid = container.querySelector('div.grid[style]') as HTMLElement
    assert.equal(grid.style.gridTemplateColumns, 'repeat(3, minmax(0, 1fr))')
    const labels = [...grid.children].map(cell => cell.textContent?.replace('Click to stream', '').trim())
    assert.deepEqual(labels, ['Left', 'Front', 'Right', '', 'Rear', '', 'Gear Guard'])
    assert.ok((grid.querySelector('video') as HTMLVideoElement).src.endsWith('09_05_26_173818__09_05_26_174959_video_sideLeft.mp4'))
  }) } finally { globalThis.fetch = oldFetch }
})

test('Viewer switches to an event category and plays, lists and deletes its clips there', async () => {
  const oldFetch = globalThis.fetch
  const toml = readFileSync(new URL('../../../profiles/rivian_roadcam.toml', import.meta.url), 'utf8')
  const rivian = {
    id: 'rivian_roadcam', display_name: 'Rivian Road Cam', segment_seconds: 900,
    filename_regex: toml.match(/^filename_regex = '(.*)'\r?$/m)![1],
    cameras: [{ id: 'frontCenter', label: 'Front' }, { id: 'gearGuard', label: 'Gear Guard', optional: true }],
    grid: [['frontCenter']],
    events: [
      { category: 'Incidents', label: 'Incidents', segment_seconds: 120 }, { category: 'Saved', label: 'Saved Clips', segment_seconds: 120 },
      { category: 'Launch', label: 'Launch Cam', segment_seconds: null },
    ],
  }
  const launch = { date: '2026-09-07_10-00-00', path: '/mutable/Recordings/Launch/2026-09-07_10-00-00', files: ['09_07_26_100000_video_frontCenter.mp4'] }
  const drive = { date: '2026-09-05', path: '/mutable/Recordings/Continuous/2026-09-05', files: ['09_05_26_173818__video_frontCenter.mp4'] }
  // One incident as Dash USB links it: its own folder, files named by their path under IncidentCam.
  const incident = {
    date: '2026-09-05_17-49-59', path: '/mutable/Recordings/Incidents/2026-09-05_17-49-59',
    files: ['frontCenter', 'gearGuard'].map(c => `TranscodedVideos__09_05_26_174959_video_${c}.mp4`),
  }
  const requests: string[] = []
  globalThis.fetch = async (input, init) => {
    const url = String(input)
    if (url === '/api/profile') return Response.json(rivian)
    if (init?.method === 'DELETE') { requests.push(`DELETE ${decodeURIComponent(url)}`); return new Response(null, { status: 204 }) }
    if (url.startsWith('/api/clips?')) {
      requests.push(url)
      const name = new URL(url, 'http://localhost').searchParams.get('category')!
      const clips = { Incidents: [incident], Continuous: [drive], Launch: [launch] }[name] ?? []
      return Response.json([{ name, clips, hasMore: false }])
    }
    return Response.json({})
  }
  try { await mount(async container => {
    const button = (text: string) => [...container.querySelectorAll('button')].find(b => b.textContent?.includes(text))!
    const pressed = () => [...container.querySelectorAll('button[aria-pressed]')].map(b => `${b.textContent}:${b.getAttribute('aria-pressed')}`)
    assert.deepEqual(pressed(), ['Recordings1:true', 'Incidents:false', 'Saved Clips:false', 'Launch Cam:false'])
    await act(async () => button('1 files').click())
    assert.ok(container.textContent?.includes('0:00 / 15:00'))

    await act(async () => button('Incidents').click())
    assert.deepEqual(pressed(), ['Recordings:false', 'Incidents1:true', 'Saved Clips:false', 'Launch Cam:false'])
    assert.ok(requests.at(-1)?.includes('category=Incidents'))
    // The drive that was playing is gone, and the incident lists by its own time.
    assert.ok(container.textContent?.includes('Select a clip to begin playback'))
    assert.ok(container.textContent?.includes('Sep 5, 5:49 PM'))
    await act(async () => button('2 files').click())
    // An incident is two minutes long, not a 15-minute drive segment.
    assert.ok(container.textContent?.includes('0:00 / 2:00'))
    const grid = container.querySelector('div.grid[style]') as HTMLElement
    assert.deepEqual([...grid.children].map(cell => cell.textContent?.replace('Click to stream', '').trim()), ['Front', 'Gear Guard'])

    await act(async () => (container.querySelector('button[title="Delete clip"]') as HTMLButtonElement).click())
    await act(async () => button('Yes').click())
    assert.equal(requests.at(-1), 'DELETE /api/files?path=/mutable/Recordings/Incidents/2026-09-05_17-49-59')

    await act(async () => button('Saved Clips').click())
    assert.ok(container.textContent?.includes('No clips here yet'))

    // A category with no known clip length borrows the drive segment length until probed.
    await act(async () => button('Launch Cam').click())
    await act(async () => button('1 files').click())
    assert.ok(container.textContent?.includes('0:00 / 15:00'))
  }) } finally { globalThis.fetch = oldFetch }
})
