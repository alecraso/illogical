// Mints a door entry link the way studio's lobby does (enterKey in box/door.mjs), from the box's enter-secret.
import { createHmac } from 'node:crypto'
const [secret, name, host, to] = process.argv.slice(2)
const e = Date.now() + 10 * 60_000
const k = createHmac('sha256', secret).update(`enter:${name}:${e}`).digest('hex')
console.log(`https://${host}/__enter?e=${e}&k=${k}${to ? `&to=${encodeURIComponent(to)}` : ''}`)
