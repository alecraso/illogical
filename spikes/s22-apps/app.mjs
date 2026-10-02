// S22: the same question as frame.mjs, through illogical's own web client: a browser block opened
// with `illogical open <entry link>` on a daemon at <app url>, in pane <pane>.
//
//   node app.mjs <app url> <pane> <label> [chrome|firefox ...]
import { createRequire } from 'node:module'
const require = createRequire(new URL('../../web/package.json', import.meta.url))
const { chromium, firefox } = require('@playwright/test')

const [app, pane, label, ...names] = process.argv.slice(2)
const engines = { chrome: () => chromium.launch({ channel: 'chrome' }), firefox: () => firefox.launch() }

for (const name of names.length ? names : ['chrome', 'firefox']) {
  const browser = await engines[name]()
  const context = await browser.newContext({ viewport: { width: 1400, height: 900 } })
  const page = await context.newPage()
  await page.goto(app)
  await page.locator('.tab').nth(1).click()
  const el = page.locator(`[data-pane="${pane}"] iframe`)
  await el.waitFor({ timeout: 20_000 })
  const sandbox = await el.getAttribute('sandbox')
  let inner
  for (let i = 0; i < 80; i++) {
    inner = page.frames().find((f) => f.url().startsWith('https://s21-app.') && new URL(f.url()).pathname === '/')
    if (inner && (await inner.evaluate(() => document.readyState).catch(() => '')) === 'complete') break
    await page.waitForTimeout(250)
  }
  const result = inner
    ? await inner.evaluate(async () => ({
        text: document.body?.innerText.trim().slice(0, 40) ?? '',
        live: await fetch('/__hud/api/live', { credentials: 'include' }).then((r) => r.status, () => 'error'),
      }))
    : { error: 'frame never reached the box' }
  await page.waitForTimeout(3000)
  await page.screenshot({ path: new URL(`results/${label}-${name}.png`, import.meta.url).pathname })
  console.log(JSON.stringify({ label, browser: name, ok: result.live === 200, sandbox, ...result }))
  await browser.close()
}
