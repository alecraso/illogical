// S22: does a studio box work inside a frame? Each case opens a fresh browser context on a fake
// top-level page (served by Playwright's router, so no server is needed) that frames the box's
// entry link, then asks, from inside the frame, whether hud's session came through.
//
//   node frame.mjs <entry link> <label> [chrome|firefox|webkit ...]
//
// Tops: `cross` is another site (as illogical's page on *.ts.net framing b-N.illogical.widgets.wtf
// is today); `same` is a sibling on the box's own site (s21-app.widgets.wtf under top.widgets.wtf).
import { createRequire } from 'node:module'
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
const require = createRequire(new URL('../../web/package.json', import.meta.url))
const { chromium, firefox, webkit } = require('@playwright/test')

const [link, label, ...names] = process.argv.slice(2)
const engines = { chrome: () => chromium.launch({ channel: 'chrome' }), firefox: () => firefox.launch(), webkit: () => webkit.launch() }
// Chrome with "Block third-party cookies" (its Incognito default), set the way editors.spec.ts sets it (#69):
// a persistent profile per case, so `newContext` hands back a fresh one.
const blocking = async () => {
  const dirs = []
  return {
    async newContext() {
      const dir = mkdtempSync(join(tmpdir(), 's22-chrome-'))
      dirs.push(dir)
      mkdirSync(join(dir, 'Default'))
      writeFileSync(join(dir, 'Default/Preferences'), JSON.stringify({ profile: { cookie_controls_mode: 1, block_third_party_cookies: true } }))
      return chromium.launchPersistentContext(dir, { channel: 'chrome' })
    },
    async close() { for (const d of dirs) rmSync(d, { recursive: true, force: true }) },
  }
}
engines['chrome-blocking'] = blocking
const tops = { cross: 'https://parent.example/', same: 'https://top.widgets.wtf/' }

for (const name of names.length ? names : ['chrome', 'firefox']) {
  const browser = await engines[name]()
  for (const [top, url] of Object.entries(tops)) {
    const context = await browser.newContext()
    const page = await context.newPage()
    const errors = []
    page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text().slice(0, 120)) })
    page.on('pageerror', (e) => errors.push(String(e).slice(0, 120)))
    await page.route(url, (route) => route.fulfill({
      contentType: 'text/html',
      body: `<!doctype html><title>top</title><iframe id=f src="${link}" style="width:1200px;height:800px"></iframe>`,
    }))
    await page.goto(url)
    // The entry link redirects twice (/__enter, /__hud/join) and lands on /: wait for the box's own page.
    let inner
    for (let i = 0; i < 60; i++) {
      inner = page.frames().find((f) => f !== page.mainFrame() && f.url().startsWith('https://s21-app.'))
      if (inner && new URL(inner.url()).pathname === '/' && (await inner.evaluate(() => document.readyState).catch(() => '')) === 'complete') break
      await page.waitForTimeout(250)
    }
    const result = inner
      ? await inner.evaluate(async () => {
          const text = document.body?.innerText.trim().slice(0, 60) ?? ''
          const live = await fetch('/__hud/api/live', { credentials: 'include' }).then((r) => r.status, () => 'error')
          const storage = (() => { try { localStorage.setItem('s22', '1'); return 'ok' } catch { return 'refused' } })()
          const panel = Boolean(document.querySelector('[data-hud], hud-root, #hud-root, [id^=hud]'))
          return { path: location.pathname, text, live, storage, panel }
        }).catch((e) => ({ error: String(e).slice(0, 80) }))
      : { error: 'frame never reached the box' }
    const cookies = (await context.cookies('https://s21-app.widgets.wtf')).map((c) => `${c.name}${c.partitionKey ? `(partitioned:${c.partitionKey})` : ''}`)
    const ok = result.live === 200
    console.log(JSON.stringify({ label, browser: name, top, ok, ...result, cookies, errors: errors.slice(0, 4) }))
    await context.close()
  }
  await browser.close()
}
