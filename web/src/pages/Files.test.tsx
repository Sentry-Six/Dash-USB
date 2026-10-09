import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import Files from './Files.tsx'

const entry = (name: string, path: string, is_dir = false) => ({ name, path, is_dir, size: 3, mod_time: '2026-10-09T18:00:00Z' })
interface FakeUpload {
  form: FormData | null; aborted: boolean
  onerror: (() => void) | null; onabort: (() => void) | null; ontimeout: (() => void) | null
  complete(status?: number, body?: unknown): void
}
interface Harness {
  container: HTMLElement; win: Window; requests: FakeUpload[]; confirmations: string[]
  setConfirmation(answer: boolean): void; setSendFailure(message: string | null): void
  setListing(reader: (path: string) => Promise<Response>): void
  file(name: string, relative?: string, size?: number): File
  pick(files: File[], folder?: boolean): Promise<void>
  button(label: string): HTMLButtonElement
  navigate(): Promise<void>
}
async function withFiles(run: (h: Harness) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/files' })
  const requests: FakeUpload[] = [], confirmations: string[] = []
  let answer = true, sendFailure: string | null = null
  let reader = async (path: string) => Response.json({ entries: path === '/mutable' ? [entry('other', '/mutable/other', true)] : [entry('current.mp4', '/mutable/other/current.mp4')] })
  class XHR {
    upload = { onprogress: null as ((event: unknown) => void) | null }
    status = 0; responseText = ''; form: FormData | null = null; aborted = false
    onload: (() => void) | null = null; onerror: (() => void) | null = null
    onabort: (() => void) | null = null; ontimeout: (() => void) | null = null
    constructor() { requests.push(this) }
    open(method: string, path: string) { assert.equal(method, 'POST'); assert.equal(path, '/api/files/upload') }
    send(form: FormData) { this.form = form; if (sendFailure) throw new Error(sendFailure) }
    abort() { this.aborted = true; this.onabort?.() }
    complete(status = 200, body: unknown = { name: 'clip.mp4', path: '/mutable/clip.mp4', size: '3' }) {
      this.status = status; this.responseText = typeof body === 'string' ? body : JSON.stringify(body); this.onload?.()
    }
  }
  const values = {
    window: win, document: win.document, navigator: win.navigator,
    FormData: win.FormData, XMLHttpRequest: XHR, IS_REACT_ACT_ENVIRONMENT: true,
    confirm: (message: string) => { confirmations.push(message); return answer },
    fetch: async (input: unknown) => String(input) === '/api/config' ? Response.json({ has_cam: 'yes' }) : reader(new URL(String(input), 'http://localhost').searchParams.get('path')!),
  }
  const previous = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container as unknown as HTMLElement)
  try {
    await act(async () => root.render(createElement(Files)))
    await run({ container: container as unknown as HTMLElement, win, requests, confirmations,
      setConfirmation: value => { answer = value }, setSendFailure: value => { sendFailure = value }, setListing: value => { reader = value },
      button: label => [...container.querySelectorAll('button')].find(b => b.textContent?.trim() === label) as unknown as HTMLButtonElement,
      file: (name, relative = '', size = 3) => { const file = new win.File(['x'.repeat(size)], name); Object.defineProperty(file, 'webkitRelativePath', { value: relative }); return file as unknown as File },
      pick: async (files, folder = false) => { const input = container.querySelector(`input[aria-label="Choose ${folder ? 'folder' : 'files'} to upload"]`)!; Object.defineProperty(input, 'files', { configurable: true, value: files }); await act(async () => input.dispatchEvent(new win.Event('change', { bubbles: true }))) },
      navigate: async () => { await act(async () => [...container.querySelectorAll('tr')].find(row => row.textContent?.includes('other'))!.click()) },
    })
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

test('folder upload preserves distinct nested paths and finishes zero-byte files', async () => {
  await withFiles(async h => {
    await h.pick([h.file('clip.mp4', 'Archive/front/clip.mp4'), h.file('clip.mp4', 'Archive/rear/clip.mp4', 0)], true)
    assert.equal(h.requests.length, 1)
    assert.equal(h.requests[0].form?.get('relative_path'), 'Archive/front/clip.mp4')
    assert.equal(h.requests[0].form?.get('path'), '/mutable')
    assert.equal(h.requests[0].form?.get('overwrite'), 'false')
    await act(async () => h.requests[0].complete())
    assert.equal(h.requests.length, 2)
    assert.equal(h.requests[1].form?.get('relative_path'), 'Archive/rear/clip.mp4')
    await act(async () => h.requests[1].complete(200, { name: 'clip.mp4', path: '/mutable/Archive/rear/clip.mp4', size: '0' }))
    assert.ok(h.container.textContent?.includes('Upload complete'))
    assert.equal(h.container.querySelector('[aria-label="Upload Archive/rear/clip.mp4"]')?.getAttribute('aria-valuenow'), '100')
  })
})

test('Retry and confirmed Replace keep the original file and destination after navigation', async () => {
  await withFiles(async h => {
    const file = h.file('clip.mp4', 'Holiday/front/clip.mp4')
    await h.pick([file], true)
    await act(async () => h.requests[0].onerror?.())
    assert.ok(h.container.textContent?.includes('Connection lost'))
    await h.navigate()
    await act(async () => h.button('Retry').click())
    assert.equal(h.requests[1].form?.get('path'), '/mutable')
    assert.equal(h.requests[1].form?.get('relative_path'), 'Holiday/front/clip.mp4')
    assert.equal(h.requests[1].form?.get('overwrite'), 'false')
    assert.equal((h.requests[1].form?.get('file') as File).name, file.name)
    await act(async () => h.requests[1].complete(409, { error: 'A file with this name already exists.' }))
    assert.ok(h.container.textContent?.includes('A file with this name already exists.'))
    h.setConfirmation(false)
    await act(async () => h.button('Replace existing file').click())
    assert.equal(h.requests.length, 2)
    assert.match(h.confirmations[0], /Holiday\/front\/clip.mp4 in \/mutable/)
    h.setConfirmation(true)
    await act(async () => h.button('Replace existing file').click())
    assert.equal(h.requests[2].form?.get('overwrite'), 'true')
    assert.equal(h.requests[2].form?.get('path'), '/mutable')
    await act(async () => h.requests[2].complete())
    assert.ok(h.container.textContent?.includes('current.mp4'))
  })
})

test('interrupted replacement requires new overwrite consent on the next conflict', async () => {
  await withFiles(async h => {
    await h.pick([h.file('clip.mp4')])
    await act(async () => h.requests[0].complete(409, { error: 'File exists' }))
    await act(async () => h.button('Replace existing file').click())
    assert.equal(h.requests[1].form?.get('overwrite'), 'true')
    await act(async () => h.requests[1].ontimeout?.())
    await act(async () => h.button('Retry').click())
    assert.equal(h.requests[2].form?.get('overwrite'), 'false')
    assert.equal(h.confirmations.length, 1)
  })
})

test('same-tick batches are admitted once and cancelled callbacks cannot restart the old queue', async () => {
  await withFiles(async h => {
    const input = h.container.querySelector('input[aria-label="Choose files to upload"]')!
    Object.defineProperty(input, 'files', { configurable: true, value: [h.file('one.mp4'), h.file('two.mp4')] })
    await act(async () => { input.dispatchEvent(new h.win.Event('change', { bubbles: true })); input.dispatchEvent(new h.win.Event('change', { bubbles: true })) })
    assert.equal(h.requests.length, 1)
    await act(async () => h.button('Cancel uploads').click())
    assert.equal(h.requests[0].aborted, true)
    await h.pick([h.file('new.mp4')])
    assert.equal(h.requests.length, 2)
    await act(async () => h.requests[0].complete())
    assert.equal(h.requests.length, 2)
    assert.ok(h.container.textContent?.includes('two.mp4'))
    await act(async () => h.requests[1].complete())
    assert.ok(h.container.textContent?.includes('Some files were not uploaded'))
    assert.ok(h.button('Dismiss upload results') || h.container.querySelector('[aria-label="Dismiss upload results"]'))
  })
})

for (const failure of ['server', 'html', 'throw', 'abort'] as const) test(`upload ${failure} failure is retained and unlocks Retry`, async () => {
  await withFiles(async h => {
    if (failure === 'throw') h.setSendFailure('XHR could not send')
    await h.pick([h.file('clip.mp4')])
    if (failure === 'server') await act(async () => h.requests[0].complete(413, { error: 'Upload exceeds the request limit' }))
    if (failure === 'html') await act(async () => h.requests[0].complete(200, '<html>app shell</html>'))
    if (failure === 'abort') await act(async () => h.requests[0].onabort?.())
    assert.ok(h.container.querySelector('[role="alert"]'))
    assert.ok(!h.container.textContent?.includes('Upload complete'))
    assert.equal(h.button('Retry').disabled, false)
    if (failure === 'server') assert.ok(h.container.textContent?.includes('Upload exceeds the request limit'))
    h.setSendFailure(null)
    await act(async () => h.button('Retry').click())
    assert.equal(h.requests.length, 2)
    await act(async () => h.requests[1].complete())
    assert.ok(h.container.textContent?.includes('Upload complete'))
  })
})

test('late upload refresh cannot replace the newly navigated directory', async () => {
  await withFiles(async h => {
    let finishRefresh: (response: Response) => void = () => {}
    h.setListing(async path => path === '/mutable' ? new Promise<Response>(resolve => { finishRefresh = resolve }) : Response.json({ entries: [entry('new-directory.mp4', '/mutable/Recordings/new-directory.mp4')] }))
    await h.pick([h.file('clip.mp4')])
    await act(async () => h.requests[0].complete())
    await act(async () => h.button('Recordings').click())
    await act(async () => finishRefresh(Response.json({ entries: [entry('stale.mp4', '/mutable/stale.mp4')] })))
    assert.ok(h.container.textContent?.includes('new-directory.mp4'))
    assert.ok(!h.container.textContent?.includes('stale.mp4'))
  })
})

test('directory drops require the folder picker instead of flattening files', async () => {
  await withFiles(async h => {
    const drop = new h.win.Event('drop', { bubbles: true, cancelable: true })
    Object.defineProperty(drop, 'dataTransfer', { value: { items: [{ webkitGetAsEntry: () => ({ isDirectory: true }) }], files: [h.file('clip.mp4')] } })
    await act(async () => h.container.querySelector('table')!.parentElement!.parentElement!.dispatchEvent(drop))
    assert.equal(h.requests.length, 0)
    assert.ok(h.container.textContent?.includes('Choose Upload Folder'))
  })
})
