// S22: a stand-in for the Anthropic Messages API, run inside the spike box on 127.0.0.1:9999, so hud's
// local agent (Claude Code through the Agent SDK) runs a real turn with no credentials in the box.
//
// A request that offers AskUserQuestion and ends on the person's own words gets a tool_use asking
// one question; one that ends on a tool_result gets the answer read back; everything else
// (titles, side requests) gets a line of text. Streams when asked to, as the SDK does.
import { createServer } from 'node:http'

const QUESTION = {
  questions: [{
    question: 'Which colour should the header be?',
    header: 'Header',
    multiSelect: false,
    options: [
      { label: 'Green (Recommended)', description: 'Matches arugula' },
      { label: 'Blue', description: 'Calmer' },
    ],
  }],
}

function reply(body) {
  // Claude Code may end the list with a system-role message (its environment): the turn's last word is before it.
  const last = (body.messages ?? []).filter((m) => m.role !== 'system').at(-1)
  const content = Array.isArray(last?.content) ? last.content : [{ type: 'text', text: String(last?.content ?? '') }]
  const result = content.find((c) => c.type === 'tool_result')
  const asks = (body.tools ?? []).some((t) => t.name === 'AskUserQuestion')
  if (result) {
    const said = Array.isArray(result.content) ? result.content.map((c) => c.text ?? '').join(' ') : String(result.content)
    return [{ type: 'text', text: `Got it: ${said.slice(0, 200)}` }]
  }
  if (asks && last?.role === 'user') return [{ type: 'tool_use', id: `toolu_s22_${Date.now()}`, name: 'AskUserQuestion', input: QUESTION }]
  return [{ type: 'text', text: 'S22 stub' }]
}

function stream(res, model, blocks) {
  const send = (type, data) => res.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...data })}\n\n`)
  res.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache' })
  send('message_start', { message: { id: `msg_${Date.now()}`, type: 'message', role: 'assistant', model, content: [], stop_reason: null, usage: { input_tokens: 10, output_tokens: 1 } } })
  blocks.forEach((b, index) => {
    if (b.type === 'text') {
      send('content_block_start', { index, content_block: { type: 'text', text: '' } })
      send('content_block_delta', { index, delta: { type: 'text_delta', text: b.text } })
    } else {
      send('content_block_start', { index, content_block: { type: 'tool_use', id: b.id, name: b.name, input: {} } })
      send('content_block_delta', { index, delta: { type: 'input_json_delta', partial_json: JSON.stringify(b.input) } })
    }
    send('content_block_stop', { index })
  })
  const stop = blocks.some((b) => b.type === 'tool_use') ? 'tool_use' : 'end_turn'
  send('message_delta', { delta: { stop_reason: stop, stop_sequence: null }, usage: { output_tokens: 20 } })
  send('message_stop', {})
  res.end()
}

createServer((req, res) => {
  let raw = ''
  req.on('data', (c) => (raw += c))
  req.on('end', () => {
    const body = raw ? JSON.parse(raw) : {}
    console.log(new Date().toISOString(), req.method, req.url, body.model ?? '', body.messages?.length ?? '', (body.tools ?? []).length)
    if (process.env.S22_TOOLS) console.log(JSON.stringify((body.messages ?? []).map((m) => [m.role, Array.isArray(m.content) ? m.content.map((c) => c.type + (c.type === 'text' ? ':' + c.text.slice(0, 40) : '')) : String(m.content).slice(0, 40)])))
    if (req.url.includes('count_tokens')) return res.writeHead(200, { 'content-type': 'application/json' }).end('{"input_tokens":10}')
    if (!req.url.includes('/v1/messages')) return res.writeHead(200, { 'content-type': 'application/json' }).end('{}')
    const blocks = reply(body)
    if (body.stream) return stream(res, body.model, blocks)
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify({
      id: `msg_${Date.now()}`, type: 'message', role: 'assistant', model: body.model, content: blocks,
      stop_reason: blocks.some((b) => b.type === 'tool_use') ? 'tool_use' : 'end_turn', stop_sequence: null,
      usage: { input_tokens: 10, output_tokens: 20 },
    }))
  })
}).listen(9999, '127.0.0.1', () => console.log('S22 stub on 127.0.0.1:9999'))
