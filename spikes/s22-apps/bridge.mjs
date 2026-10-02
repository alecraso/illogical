// S22: a studio box's open questions as illogical asks, both ways.
//
// Runs in an illogical terminal pane ($ILLOGICAL_PANE; the daemon's ask route takes terminals only),
// beside the box's browser block. For every chat tab of the box's hud it follows
// /__hud/api/chat/stream; when a turn waits on a question (the `question` of a `hud-chat-queue`
// frame), it puts it to illogical as AskUserQuestion's card (POST /api/panes/<pane>/ask, a long
// poll), so it is an M24 `ask` reason: on every client, the swarm's rail, push, `illogical attention`.
// An answer there goes back as /__hud/api/chat/answer; an answer in hud's own panel (or the question
// expiring) withdraws the card.
//
//   ILLOGICAL_SOCK=… ILLOGICAL_PANE=N node bridge.mjs https://<box> <cookie file>
//
// The cookie is a hud session (the owner's, here): hud records whoever answers as that player.
import { readFileSync, appendFileSync } from 'node:fs'
import { request } from 'node:http'

const [box, cookieFile] = process.argv.slice(2)
const cookie = readFileSync(cookieFile, 'utf8').trim()
const sock = process.env.ILLOGICAL_SOCK
const pane = Number(process.env.ILLOGICAL_PANE)
const t0 = Date.now()
const log = (event, data = {}) => {
  const line = JSON.stringify({ ms: Date.now(), event, ...data })
  console.log(line)
  if (process.env.S22_LOG) appendFileSync(process.env.S22_LOG, line + '\n')
}

/** illogicald over its socket. */
function daemon(method, path, body) {
  return new Promise((resolve, reject) => {
    const req = request({ socketPath: sock, method, path, headers: { 'content-type': 'application/json' } }, (res) => {
      let raw = ''
      res.on('data', (c) => (raw += c))
      res.on('end', () => resolve({ status: res.statusCode, body: raw ? JSON.parse(raw) : null }))
    })
    req.on('error', reject)
    req.end(body ? JSON.stringify(body) : undefined)
  })
}

const hud = (path, init = {}) =>
  fetch(`${box}/__hud${path}`, { ...init, headers: { cookie, origin: box, 'content-type': 'application/json', ...init.headers } })

/** requestId → { chatKey, question } for each card this bridge has open. */
const open = new Map()

async function ask(chatKey, q) {
  if (open.has(q.requestId)) return
  open.set(q.requestId, { chatKey, q })
  // hud's question as AskUserQuestion's: one single-select question, its options by name.
  const questions = [{
    question: q.summary,
    header: 'hud',
    multiSelect: false,
    options: q.options.map((o) => ({ label: o.name, ...(o.description ? { description: o.description } : {}) })),
  }]
  log('asked', { requestId: q.requestId, hudAskedAt: q.askedAt, lagMs: Date.now() - q.askedAt })
  const r = await daemon('POST', `/api/panes/${pane}/ask`, { questions, id: q.requestId }).catch((e) => ({ status: 0, body: { error: String(e) } }))
  open.delete(q.requestId)
  const action = r.body?.action
  if (action !== 'accept') return log('card-ended', { requestId: q.requestId, action: action ?? r.body?.error })
  const label = r.body.output?.hookSpecificOutput?.updatedInput?.answers?.[q.summary]
  const option = q.options.find((o) => o.name === label)
  if (!option) return log('unmatched', { requestId: q.requestId, label })
  const sent = Date.now()
  const a = await hud('/api/chat/answer', { method: 'POST', body: JSON.stringify({ chatKey, requestId: q.requestId, optionId: option.optionId }) })
  log('answered-in-illogical', { requestId: q.requestId, optionId: option.optionId, hud: a.status, hudMs: Date.now() - sent, body: (await a.text()).slice(0, 200) })
}

async function withdraw(requestId, why) {
  if (!open.has(requestId)) return
  open.delete(requestId)
  const r = await daemon('POST', `/api/panes/${pane}/ask/withdraw`, { id: requestId })
  log('withdrawn', { requestId, why, status: r.status })
}

async function follow(chatKey) {
  for (;;) {
    try {
      const res = await hud(`/api/chat/stream?chatKey=${encodeURIComponent(chatKey)}`)
      log('following', { chatKey, status: res.status })
      if (!res.ok) throw new Error(`stream ${res.status}`)
      const decoder = new TextDecoder()
      let buf = ''
      for await (const chunk of res.body) {
        buf += decoder.decode(chunk, { stream: true })
        let at
        while ((at = buf.indexOf('\n\n')) >= 0) {
          const frame = buf.slice(0, at)
          buf = buf.slice(at + 2)
          const data = frame.split('\n').filter((l) => l.startsWith('data: ')).map((l) => l.slice(6)).join('')
          if (!data) continue
          const m = JSON.parse(data)
          if (m.type === 'hud-chat-queue') {
            const q = m.question
            // A question gone from the queue was answered elsewhere, expired or interrupted.
            for (const [id, o] of open) if (o.chatKey === chatKey && id !== q?.requestId) void withdraw(id, 'gone from hud')
            if (q) void ask(chatKey, q)
          }
          if (m.kind === 'permission_request' && m.answered) void withdraw(m.id, `answered in hud by ${m.answeredBy?.name ?? '?'}`)
          if (m.block?.kind === 'permission_request' && m.block.answered) void withdraw(m.block.id, `answered in hud by ${m.block.answeredBy?.name ?? '?'}`)
        }
      }
    } catch (e) {
      log('stream-error', { chatKey, error: String(e).slice(0, 120) })
    }
    await new Promise((r) => setTimeout(r, 2000))
  }
}

const tabs = await (await hud('/api/tabs')).json()
log('start', { box, pane, tabs: tabs.tabs.map((t) => t.chatKey), startMs: Date.now() - t0 })
for (const t of tabs.tabs) void follow(t.chatKey)
