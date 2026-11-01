import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement } from 'react'
import { Window } from 'happy-dom'
import { AISupportChat } from './AISupportChat.tsx'
import { SUPPORT_DISCLOSURE_VERSION, type SupportMessage, type SupportTransport } from '../../api/support.ts'

const request = {
  kind: 'diagnostics',
  label: 'Dash USB diagnostics with USB, storage, network, and recent logs',
  fileName: 'dashusb-diagnostics.txt',
  reason: 'Review the disclosed diagnostic categories before approving.',
  maxBytes: 2 * 1024 * 1024,
  retentionDays: 7,
}

function fakeTransport(calls: string[]): SupportTransport {
  const assistant: SupportMessage = {
    id: '018f42a0-7f52-4b3c-9a11-0123456789ab',
    role: 'assistant',
    content: 'The **diagnostics report** can show whether the recorder is writing.',
    createdAt: new Date().toISOString(),
    actions: { fileRequests: [request], suggestDiscord: true },
  }
  return {
    async createConversation(message) {
      calls.push(`create:${message}`)
      return {
        conversation: { id: '018f42a0-7f52-4b3c-9a11-0123456789aa', authToken: 'token-123456789012345' },
        messages: [{ id: 'server-user', role: 'user', content: message, createdAt: new Date().toISOString() }, assistant],
        status: 'idle',
      }
    },
    async fetchMessages() { calls.push('fetch'); return { messages: [], status: 'idle' } },
    async sendMessage(_conversation, content) { calls.push(`send:${content}`); return { messages: [], status: 'idle' } },
    async deleteConversation() { calls.push('delete') },
    async decideFileRequest(_conversation, reference, decision) {
      calls.push(`decide:${reference.request.fileName}:${decision}`)
      return decision === 'approved' ? { decision, uploadToken: 'upload-token', maxBytes: request.maxBytes } : { decision }
    },
    async collectDiagnostics(maxBytes) { calls.push(`collect:${maxBytes}`); return new Blob(['report'], { type: 'text/plain' }) },
    async uploadRequestedFile(_conversation, reference) {
      calls.push(`upload:${reference.request.fileName}`)
      return { fileName: reference.request.fileName, size: 6, retentionDays: 7 }
    },
  }
}

async function withChat(check: (container: HTMLElement, calls: string[], win: Window) => Promise<void>) {
  const win = new Window({ url: 'http://dashusb.local/support' })
  win.HTMLElement.prototype.scrollIntoView = () => {}
  const values = { window: win, document: win.document, navigator: win.navigator, localStorage: win.localStorage, IS_REACT_ACT_ENVIRONMENT: true }
  const descriptors = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const calls: string[] = []
  const { createRoot } = await import('react-dom/client')
  const container = win.document.createElement('div'); win.document.body.append(container)
  const root = createRoot(container)
  try {
    await act(async () => root.render(createElement(AISupportChat, { transport: fakeTransport(calls) })))
    await check(container as unknown as HTMLElement, calls, win)
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of descriptors) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

const button = (container: HTMLElement, text: string) =>
  [...container.querySelectorAll('button')].find(item => item.textContent?.includes(text)) as HTMLButtonElement | undefined

async function typeAndSend(container: HTMLElement, win: Window, text: string) {
  const textarea = container.querySelector('textarea') as HTMLTextAreaElement
  const setter = Object.getOwnPropertyDescriptor(win.HTMLTextAreaElement.prototype, 'value')!.set!
  await act(async () => {
    setter.call(textarea, text)
    textarea.dispatchEvent(new win.Event('input', { bubbles: true }) as unknown as Event)
  })
  await act(async () => (container.querySelector('button[aria-label="Send message"]') as HTMLButtonElement).click())
}

test('the disclosure describes AI Support before any message can be sent', async () => {
  await withChat(async (container, calls) => {
    const text = container.textContent ?? ''
    assert.match(text, /online AI assistant/)
    assert.match(text, /Anthropic/)
    assert.match(text, /Canada for up to 90 days/)
    assert.match(text, /approve a request, and are deleted within 7 days/)
    assert.equal(container.querySelector('textarea'), null)
    assert.ok(container.querySelector('a[href="https://discord.gg/9QZEzVwdnt"]'))
    assert.ok(container.querySelector('a[href="https://sentry-six.com/privacy"]'))
    assert.deepEqual(calls, [])
  })
})

test('a chat starts after acknowledgement and diagnostics upload only after approval', async () => {
  await withChat(async (container, calls, win) => {
    await act(async () => button(container, 'Acknowledge')!.click())
    await typeAndSend(container, win, 'My recorder stopped writing')
    const work = () => calls.filter(call => call !== 'fetch')
    assert.deepEqual(work(), ['create:My recorder stopped writing'])
    const stored = JSON.parse(win.localStorage.getItem('dashusb_ai_support_conversation') ?? '{}')
    assert.equal(stored.disclosureVersion, SUPPORT_DISCLOSURE_VERSION)
    assert.match(container.textContent ?? '', /AI requests one-time access/)
    assert.match(container.textContent ?? '', /Want help from a person\?/)
    assert.equal(container.querySelector('strong')?.textContent, 'diagnostics report')

    await act(async () => button(container, 'Generate & upload diagnostics once')!.click())
    assert.deepEqual(work().slice(1), [
      'decide:dashusb-diagnostics.txt:approved',
      `collect:${request.maxBytes}`,
      'upload:dashusb-diagnostics.txt',
    ])
    assert.match(container.textContent ?? '', /was shared once/)
  })
})

test('New chat deletes the server conversation only after confirmation', async () => {
  await withChat(async (container, calls, win) => {
    await act(async () => button(container, 'Acknowledge')!.click())
    await typeAndSend(container, win, 'Hello')
    let confirmed = false
    win.confirm = () => confirmed
    await act(async () => button(container, 'New chat')!.click())
    assert.equal(calls.includes('delete'), false)
    confirmed = true
    await act(async () => button(container, 'New chat')!.click())
    assert.equal(calls.at(-1), 'delete')
    assert.equal(win.localStorage.getItem('dashusb_ai_support_conversation'), null)
    assert.match(container.textContent ?? '', /Before you chat/)
  })
})
