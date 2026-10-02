// S22: the whole loop in illogical's own client. With bridge.mjs running in pane <bridge> beside the
// box's block, a prompt makes the box's agent ask; this screenshots the tab and the swarm, answers
// by clicking the card on the swarm's rail, and checks hud saw that answer.
//
//   node demo.mjs <app url> <box> <cookie file> <chatKey> <bridge pane>
import { readFileSync } from 'node:fs'
import { createRequire } from 'node:module'
const require = createRequire(new URL('../../web/package.json', import.meta.url))
const { chromium } = require('@playwright/test')

const [app, box, cookieFile, chatKey, bridge] = process.argv.slice(2)
const cookie = readFileSync(cookieFile, 'utf8').trim()
const out = (name) => new URL(`results/${name}.png`, import.meta.url).pathname
const hud = (path, body) => fetch(`${box}/__hud${path}`, {
  method: body ? 'POST' : 'GET', body: body && JSON.stringify(body),
  headers: { cookie, origin: box, 'content-type': 'application/json' },
}).then((r) => r.json())

const browser = await chromium.launch({ channel: 'chrome' })
const page = await (await browser.newContext({ viewport: { width: 1400, height: 900 } })).newPage()
await page.goto(app)
await page.locator('.tab').nth(1).click()

await hud('/api/chat/prompt', { chatKey, text: 'Pick a header colour' })
const card = page.locator(`[data-pane="${bridge}"]`).getByRole('dialog', { name: 'Which colour should the header be?' })
await card.waitFor({ timeout: 20_000 })
await page.waitForTimeout(1500)
await page.screenshot({ path: out('tab-with-question') })

await page.locator('.tab', { hasText: 'Swarm' }).first().click().catch(() => page.getByText('Swarm').first().click())
await page.waitForTimeout(2500)
await page.screenshot({ path: out('swarm-with-question') })

// Answered from the swarm's rail, where anyone on the team who may answer sees it.
const rail = page.locator('.swarm-rail').getByRole('dialog', { name: 'Which colour should the header be?' })
await rail.getByRole('radio', { name: /^Blue/ }).click()
await rail.getByRole('button', { name: 'Submit' }).click()
await rail.waitFor({ state: 'detached', timeout: 10_000 })
await page.waitForTimeout(1500)

// What hud recorded: the question's settled block in the conversation.
const stream = await fetch(`${box}/__hud/api/chat/stream?chatKey=${chatKey}`, { headers: { cookie } })
const reader = stream.body.getReader()
let text = ''
const until = Date.now() + 4000
while (Date.now() < until && !text.includes('hud-chat-presence')) text += new TextDecoder().decode((await reader.read()).value)
await reader.cancel()
const queue = text.split('\n').filter((l) => l.includes('"hud-chat-queue"')).at(-1) ?? ''
console.log(JSON.stringify({ cardGone: true, hudStillAsking: queue.includes('"question"') }))
await page.screenshot({ path: out('swarm-answered') })
await browser.close()
