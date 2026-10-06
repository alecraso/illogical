// A message's text, with a team chat's small Markdown (M74): fenced code
// blocks, `code`, **bold**, *italic* / _italic_, links, and @mentions as
// pills. Only Preact nodes come out, never HTML: whatever someone types
// stays text.

import type { ComponentChildren } from "preact";

/** Whether a mention (`@jake`) is the person reading. */
export type IsMe = (token: string) => boolean;

export function Markup({ text, isMe }: { text: string; isMe?: IsMe }) {
  const out: ComponentChildren[] = [];
  // Fenced blocks first: nothing inside them is formatted.
  const fence = /```[^\n]*\n?([\s\S]*?)```/g;
  let at = 0;
  let k = 0;
  for (let m = fence.exec(text); m; m = fence.exec(text)) {
    if (m.index > at) out.push(<Lines key={k++} text={text.slice(at, m.index)} isMe={isMe} />);
    out.push(
      <pre key={k++} class="msg-code">
        {m[1].replace(/\n$/, "")}
      </pre>,
    );
    at = m.index + m[0].length;
  }
  if (at < text.length) out.push(<Lines key={k++} text={text.slice(at)} isMe={isMe} />);
  return <>{out}</>;
}

/** Text between code blocks, trimmed of the newlines around the blocks. */
function Lines({ text, isMe }: { text: string; isMe?: IsMe }) {
  const t = text.replace(/^\n/, "").replace(/\n$/, "");
  if (!t) return null;
  return <p class="thread-text">{inline(t, isMe)}</p>;
}

// One pattern for every inline form; the first group that matched says
// which. Order matters: code before the rest, so `**x**` in code stays.
const INLINE =
  /(`[^`\n]+`)|(\*\*[^*\n]+\*\*)|((?:^|(?<=[\s(]))[*_][^*_\s][^*_\n]*[*_](?=$|[\s.,;:!?)]))|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])|((?:^|(?<=[^\w]))@[\w.-]*\w)/g;

export function inline(text: string, isMe?: IsMe): ComponentChildren[] {
  const out: ComponentChildren[] = [];
  let at = 0;
  let k = 0;
  for (const m of text.matchAll(INLINE)) {
    const i = m.index!;
    if (i > at) out.push(text.slice(at, i));
    const s = m[0];
    if (m[1]) out.push(<code key={k++}>{s.slice(1, -1)}</code>);
    else if (m[2]) out.push(<b key={k++}>{inline(s.slice(2, -2), isMe)}</b>);
    else if (m[3]) out.push(<i key={k++}>{inline(s.slice(1, -1), isMe)}</i>);
    else if (m[4])
      out.push(
        <a key={k++} href={s} target="_blank" rel="noopener noreferrer">
          {s}
        </a>,
      );
    else {
      const token = s.slice(1).toLowerCase();
      out.push(
        <span key={k++} class={isMe?.(token) ? "mention me" : "mention"}>
          {s}
        </span>,
      );
    }
    at = i + s.length;
  }
  if (at < text.length) out.push(text.slice(at));
  return out;
}

/** The token that @mentions a person, as the daemon matches it
 * (threads.rs `names`): their first name, or the part before the `@`. */
export function mentionToken(name: string): string {
  const first = name.trim().split(/\s+/)[0] ?? "";
  return first.split("@")[0].toLowerCase().replace(/[^\w.-]/g, "");
}
