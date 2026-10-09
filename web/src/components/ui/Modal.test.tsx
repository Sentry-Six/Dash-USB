import assert from 'node:assert/strict'
import test from 'node:test'
import { act, createElement as el, StrictMode, useState, type ReactNode, type ChangeEvent, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import { Window } from 'happy-dom'
import { Modal } from './Modal.tsx'

async function withDocument(run: (h: { win: Window; app: HTMLElement; opener: HTMLButtonElement; originalInert: HTMLElement; render(content: ReactNode): Promise<void>; dialog(title: string): HTMLElement; key(key: string, shift?: boolean): KeyboardEvent }) => Promise<void>) {
  const win = new Window({ url: 'http://localhost/' })
  const values = { window: win, document: win.document, navigator: win.navigator, IS_REACT_ACT_ENVIRONMENT: true }
  const previous = Object.keys(values).map(key => [key, Object.getOwnPropertyDescriptor(globalThis, key)] as const)
  for (const [key, value] of Object.entries(values)) Object.defineProperty(globalThis, key, { configurable: true, value })
  const app = win.document.createElement('div'), opener = win.document.createElement('button'), originalInert = win.document.createElement('div')
  opener.textContent = 'Open dialog'; originalInert.inert = true
  app.append(opener); win.document.body.append(app, originalInert)
  win.document.body.style.setProperty('overflow', 'clip', 'important'); opener.focus()
  const { createRoot } = await import('react-dom/client')
  const root = createRoot(app.appendChild(win.document.createElement('div')) as unknown as HTMLElement)
  try {
    await run({ win, app: app as unknown as HTMLElement, opener: opener as unknown as HTMLButtonElement, originalInert: originalInert as unknown as HTMLElement,
      render: async content => { await act(async () => root.render(content)) },
      dialog: title => [...win.document.querySelectorAll<HTMLElement>('[role="dialog"]')].find(node => win.document.getElementById(node.getAttribute('aria-labelledby')!)?.textContent === title) as unknown as HTMLElement,
      key: (key, shiftKey = false) => { const event = new win.KeyboardEvent('keydown', { key, shiftKey, bubbles: true, cancelable: true }); win.document.activeElement!.dispatchEvent(event); return event as unknown as KeyboardEvent },
    })
  } finally {
    await act(async () => root.unmount())
    for (const [key, descriptor] of previous) { if (descriptor) Object.defineProperty(globalThis, key, descriptor); else Reflect.deleteProperty(globalThis, key) }
    win.close()
  }
}

test('dialog labels itself and wraps focus past hidden, negative-tabindex and disabled controls', async () => {
  await withDocument(async h => {
    await h.render(el(Modal, { title: 'Archive details', onClose() {}, children: el('div', null,
      el('input', { type: 'hidden' }), el('button', { tabIndex: -1 }, 'Not tabbable'),
      el('fieldset', { disabled: true }, el('button', null, 'Disabled')),
      el('button', { style: { display: 'none' } }, 'Hidden'), el('button', { id: 'last' }, 'Last action')) }))
    const dialog = h.dialog('Archive details'), close = dialog.querySelector<HTMLButtonElement>('[aria-label="Close"]')!, last = dialog.querySelector<HTMLButtonElement>('#last')!
    assert.equal(dialog.getAttribute('aria-modal'), 'true'); assert.equal(h.win.document.activeElement, close)
    last.focus(); assert.equal(h.key('Tab').defaultPrevented, true); assert.equal(h.win.document.activeElement, close)
    assert.equal(h.key('Tab', true).defaultPrevented, true); assert.equal(h.win.document.activeElement, last)
    await h.render(null); assert.equal(h.win.document.activeElement, h.opener)
  })
})

test('background focus and clicks are blocked including late content; inert and scroll styles are restored', async () => {
  await withDocument(async h => {
    let clicks = 0; h.opener.addEventListener('click', () => { clicks++ })
    await h.render(el(Modal, { title: 'Health', onClose() {}, children: el('button', null, 'Check') }))
    assert.equal(h.app.inert, true); assert.equal(h.originalInert.inert, true); assert.equal(h.win.document.body.style.overflow, 'hidden')
    const added = h.win.document.createElement('button'); added.textContent = 'Late button'; added.addEventListener('click', () => { clicks++ })
    await act(async () => { h.win.document.body.append(added); await new Promise(resolve => setTimeout(resolve, 0)) })
    assert.equal(added.inert, true)
    h.opener.click(); added.click(); h.opener.focus()
    assert.equal(clicks, 0); assert.ok(h.dialog('Health').contains(h.win.document.activeElement as unknown as Node))
    await h.render(null)
    assert.equal(h.app.inert, false); assert.equal(h.originalInert.inert, true); assert.equal(added.inert, false)
    assert.equal(h.win.document.body.style.overflow, 'clip'); assert.equal(h.win.document.body.style.getPropertyPriority('overflow'), 'important')
    assert.equal(h.win.document.activeElement, h.opener)
  })
})

test('only top nested dialog consumes Escape; closing it restores its parent trigger', async () => {
  await withDocument(async h => {
    let outerClosed = 0
    function Nested() {
      const [inner, setInner] = useState(false)
      return el(Modal, { title: 'Outer', onClose: () => { outerClosed++ }, children: el('div', null,
        el('button', { id: 'open-inner', onClick: () => setInner(true) }, 'More details'),
        inner ? el(Modal, { title: 'Inner', onClose: () => setInner(false), children: el('button', null, 'Inner action') }) : null) })
    }
    await h.render(el(Nested))
    const trigger = h.dialog('Outer').querySelector<HTMLButtonElement>('#open-inner')!
    trigger.focus(); await act(async () => trigger.click())
    const inner = h.dialog('Inner'), outer = h.dialog('Outer')
    assert.equal(outer.inert, true); assert.equal(inner.inert, false); assert.ok(Number(inner.style.zIndex) > Number(outer.style.zIndex))
    await act(async () => { h.key('Escape') })
    assert.equal(outerClosed, 0); assert.equal(h.dialog('Inner'), undefined); assert.equal(outer.inert, false); assert.equal(h.win.document.activeElement, trigger)
    await act(async () => { h.key('Escape') }); assert.equal(outerClosed, 1)
    await act(async () => trigger.click())
    await h.render(null)
    assert.equal(h.win.document.activeElement, h.opener, 'Removing parent and child together restores the original opener')
  })
})

test('initially nested dialogs stack correctly and whole-stack cleanup restores original opener', async () => {
  await withDocument(async h => {
    let closed = ''
    await h.render(el(Modal, { title: 'Parent', onClose: () => { closed = 'parent' }, children:
      el(Modal, { title: 'Child', onClose: () => { closed = 'child' }, children: el('button', null, 'Action') }) }))
    assert.equal(h.dialog('Parent').inert, true); assert.equal(h.dialog('Child').inert, false)
    assert.ok(h.dialog('Child').contains(h.win.document.activeElement as unknown as Node))
    await act(async () => { h.key('Escape') }); assert.equal(closed, 'child')
    await h.render(null); assert.equal(h.app.inert, false); assert.equal(h.win.document.activeElement, h.opener)
  })
})

test('nondismissable dialogs trap focus and ignore Escape and backdrop clicks', async () => {
  await withDocument(async h => {
    let closed = 0
    await h.render(el(Modal, { title: 'Installing', dismissable: false, onClose: () => { closed++ }, children: el('p', null, 'Working…') }))
    const dialog = h.dialog('Installing')
    assert.equal(dialog.querySelector('button'), null); assert.equal(h.win.document.activeElement, dialog)
    assert.equal(h.key('Tab').defaultPrevented, true); assert.equal(h.key('Tab', true).defaultPrevented, true)
    await act(async () => { h.key('Escape'); dialog.click() })
    assert.equal(closed, 0); assert.equal(h.win.document.activeElement, dialog)
    await h.render(el(Modal, { title: 'Installing', dismissable: true, onClose: () => { closed++ }, children: el('p', null, 'Finished') }))
    await act(async () => { h.key('Escape') })
    assert.equal(closed, 1, 'The latest dismissable value applies without remounting')
  })
})

test('closing a lower sibling dialog preserves focus and caret in the top dialog', async () => {
  await withDocument(async h => {
    const view = (lower: boolean) => el('div', null,
      lower ? el(Modal, { key: 'lower', title: 'Lower', onClose() {}, children: el('button', null, 'Lower action') }) : null,
      el(Modal, { key: 'upper', title: 'Upper', onClose() {}, children: el('input', { id: 'editing', defaultValue: 'archive' }) }))
    await h.render(view(true))
    const input = h.dialog('Upper').querySelector<HTMLInputElement>('#editing')!
    input.focus(); input.setSelectionRange(1, 4)
    await h.render(view(false))
    assert.equal(h.win.document.activeElement, input)
    assert.equal(input.selectionStart, 1); assert.equal(input.selectionEnd, 4)
  })
})

test('native select controls and consumed dropdown keys remain usable', async () => {
  await withDocument(async h => {
    let closed = 0, selected = ''
    await h.render(el(Modal, { title: 'Options', onClose: () => { closed++ }, children: el('div', null,
      el('select', { id: 'choice', onChange: (event: ChangeEvent<HTMLSelectElement>) => { selected = event.target.value } }, el('option', { value: 'one' }, 'One'), el('option', { value: 'two' }, 'Two')),
      el('button', { id: 'dropdown', onKeyDown: (event: ReactKeyboardEvent) => { if (event.key === 'Escape') event.preventDefault() } }, 'Dropdown')) }))
    const select = h.dialog('Options').querySelector<HTMLSelectElement>('#choice')!
    select.focus(); assert.equal(h.key('ArrowDown').defaultPrevented, false)
    await act(async () => { select.value = 'two'; select.dispatchEvent(new h.win.Event('change', { bubbles: true })); h.key('Escape') })
    assert.equal(selected, 'two'); assert.equal(closed, 0)
    h.dialog('Options').querySelector<HTMLButtonElement>('#dropdown')!.focus()
    await act(async () => { h.key('Escape') }); assert.equal(closed, 0)
    h.dialog('Options').querySelector<HTMLButtonElement>('[aria-label="Close"]')!.focus()
    await act(async () => { h.key('Escape') }); assert.equal(closed, 1)
  })
})

test('StrictMode and callback updates do not duplicate handlers or lose restoration', async () => {
  await withDocument(async h => {
    let oldClosed = 0, newClosed = 0
    await h.render(el(StrictMode, null, el(Modal, { title: 'Strict', onClose: () => { oldClosed++ }, children: el('input', { autoFocus: true }) })))
    await h.render(el(StrictMode, null, el(Modal, { title: 'Strict', onClose: () => { newClosed++ }, children: el('input', { autoFocus: true }) })))
    assert.equal(h.app.inert, true)
    await act(async () => { h.key('Escape') }); assert.equal(oldClosed, 0); assert.equal(newClosed, 1)
    await h.render(null)
    assert.equal(h.app.inert, false); assert.equal(h.originalInert.inert, true)
    assert.equal(h.win.document.body.style.overflow, 'clip'); assert.equal(h.win.document.activeElement, h.opener)
  })
})
