// Questions and forms from agents (M6c), as cards: Claude's
// AskUserQuestion as a question card (buttons for one answer, checkboxes for
// several, an "Other" box, option previews in monospace), any other form
// drawn from its JSON Schema, and a sign-in link. The same card shows in an
// agent block and beside a terminal running Claude Code (through its hook).
// Answers are the form's fields either way: `question_<n>` (a label, or a
// list of them) and `question_<n>_custom` for a question card.

import { useState } from "preact/hooks";

export interface AskOption {
  label: string;
  description?: string;
  preview?: string;
}

export interface Question {
  question: string;
  header?: string;
  multiSelect?: boolean;
  options: AskOption[];
}

export interface Ask {
  id: string;
  kind: "questions" | "form" | "url" | "permission";
  message: string;
  questions?: Question[];
  schema?: Schema;
  url?: string;
  accepted?: boolean;
  tool_call_id?: string;
  /** `agent`, `hook`, or what raised it on a block (M35: `hud`). */
  source: string;
  /** Who asks, when that isn't the terminal's own agent (M35: "hud"). */
  agent?: string;
  at_ms: number;
  /** `permission` (M29): the tool Claude Code asks to use, its input and
   * its own "always allow" suggestions. */
  tool?: string;
  input?: Record<string, unknown>;
  suggestions?: Suggestion[];
  session?: string;
}

/** One of Claude Code's "always allow" suggestions, as its hook gives it. */
export interface Suggestion {
  type: string;
  rules?: { toolName: string; ruleContent?: string }[];
  directories?: string[];
  mode?: string;
  destination?: string;
}

/** Who answered a card, and how (M29). */
export interface Answered {
  id: string;
  how: string;
  who: string;
  name: string;
  at_ms: number;
  headline: string;
}

/** The JSON Schema subset forms use (MCP's, Codex's, Claude's). */
export interface Schema {
  type?: string;
  title?: string;
  description?: string;
  properties?: Record<string, Schema>;
  required?: string[];
  enum?: (string | number)[];
  enumNames?: string[];
  oneOf?: { const: string | number; title?: string; description?: string }[];
  anyOf?: { const: string | number; title?: string; description?: string }[];
  items?: Schema;
  minimum?: number;
  maximum?: number;
  maxLength?: number;
  /** `markdown` (M36: a forge draft's text): a multi-line box. */
  format?: string;
  default?: unknown;
}

export interface AskActions {
  answer(content: Record<string, unknown>): void;
  decline(): void;
  /** Hook questions: let Claude Code show its own picker. */
  terminal?(): void;
  /** Agent blocks: stop the turn (the agent withdraws the question). */
  stop?(): void;
}

/** What a list or a notification says about it. */
export function headline(a: Ask): string {
  return a.questions?.[0]?.question ?? a.message;
}

export function AskCard({ ask, actions }: { ask: Ask; actions: AskActions }) {
  return (
    <div class="ask" role="dialog" aria-label={headline(ask)} data-ask={ask.id}>
      {ask.kind === "questions" && ask.questions ? (
        <QuestionCard ask={ask} questions={ask.questions} actions={actions} />
      ) : ask.kind === "url" ? (
        <LinkCard ask={ask} actions={actions} />
      ) : (
        <FormCard ask={ask} actions={actions} />
      )}
    </div>
  );
}

function Buttons({
  actions,
  submit,
  disabled,
  labels,
}: {
  actions: AskActions;
  submit?: () => void;
  disabled?: boolean;
  labels?: { submit: string; decline: string };
}) {
  return (
    <div class="ask-buttons">
      {submit && (
        <button class="primary" data-ask-submit disabled={disabled} onClick={submit}>
          {labels?.submit ?? "Submit"}
        </button>
      )}
      <button data-ask-decline onClick={() => actions.decline()}>
        {labels?.decline ?? "Skip"}
      </button>
      {actions.terminal && (
        <button title="Claude Code shows its own picker" onClick={() => actions.terminal!()}>
          Answer in terminal
        </button>
      )}
      {actions.stop && (
        <button class="danger" onClick={() => actions.stop!()}>
          Stop
        </button>
      )}
    </div>
  );
}

// ------------------------------------------------------------ question card

function QuestionCard({ ask, questions, actions }: { ask: Ask; questions: Question[]; actions: AskActions }) {
  const [picks, setPicks] = useState<string[][]>(() => questions.map(() => []));
  const [other, setOther] = useState<string[]>(() => questions.map(() => ""));
  const [preview, setPreview] = useState<{ n: number; text: string } | null>(null);
  const toggle = (n: number, q: Question, label: string) => {
    const cur = picks[n];
    const next = q.multiSelect ? (cur.includes(label) ? cur.filter((l) => l !== label) : [...cur, label]) : cur[0] === label ? [] : [label];
    setPicks(picks.map((p, i) => (i === n ? next : p)));
  };
  const answered = questions.some((_, n) => picks[n].length > 0 || other[n].trim() !== "");
  const submit = () => {
    const content: Record<string, unknown> = {};
    questions.forEach((q, n) => {
      if (picks[n].length) content[`question_${n}`] = q.multiSelect ? picks[n] : picks[n][0];
      if (other[n].trim()) content[`question_${n}_custom`] = other[n].trim();
    });
    actions.answer(content);
  };
  return (
    <>
      {questions.length > 1 && <div class="ask-message">{ask.message}</div>}
      {questions.map((q, n) => (
        <fieldset key={n} class="ask-question" data-question={n}>
          <legend>
            {q.header && <span class="ask-header">{q.header}</span>}
            <span class="ask-text">{q.question}</span>
            {q.multiSelect && <span class="ask-hint">any of these</span>}
          </legend>
          <div class={q.multiSelect ? "ask-options multi" : "ask-options"} role={q.multiSelect ? "group" : "radiogroup"}>
            {q.options.map((o) => {
              const on = picks[n].includes(o.label);
              const show = () => o.preview && setPreview({ n, text: o.preview });
              return q.multiSelect ? (
                <label key={o.label} class={on ? "ask-option on" : "ask-option"} onFocusIn={show} onPointerDown={show}>
                  <input type="checkbox" checked={on} onChange={() => toggle(n, q, o.label)} />
                  <span class="ask-label">{o.label}</span>
                  {o.description && <span class="ask-desc">{o.description}</span>}
                </label>
              ) : (
                <button
                  key={o.label}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  class={on ? "ask-option on" : "ask-option"}
                  onFocus={show}
                  onClick={() => {
                    show();
                    toggle(n, q, o.label);
                  }}
                >
                  <span class="ask-label">{o.label}</span>
                  {o.description && <span class="ask-desc">{o.description}</span>}
                </button>
              );
            })}
          </div>
          {preview?.n === n && <pre class="ask-preview">{preview.text}</pre>}
          <input
            class="ask-other"
            type="text"
            aria-label={`Other: ${q.question}`}
            placeholder={q.multiSelect ? "Other (added to your picks)" : picks[n].length ? "A note on your answer (optional)" : "Other"}
            value={other[n]}
            onInput={(e) => {
              const v = (e.currentTarget as HTMLInputElement).value;
              setOther(other.map((x, i) => (i === n ? v : x)));
            }}
            onKeyDown={(e) => e.key === "Enter" && answered && submit()}
          />
        </fieldset>
      ))}
      <Buttons actions={actions} submit={submit} disabled={!answered} />
    </>
  );
}

// ------------------------------------------------------------ generic form

interface Choice {
  value: string | number;
  label: string;
  description?: string;
}

/** A field's choices: `oneOf`/`anyOf` consts, or `enum` with `enumNames`. */
function choices(f: Schema | undefined): Choice[] | null {
  if (!f) return null;
  const consts = f.oneOf ?? f.anyOf;
  if (consts) return consts.map((c) => ({ value: c.const, label: c.title ?? String(c.const), description: c.description }));
  if (f.enum) return f.enum.map((v, i) => ({ value: v, label: f.enumNames?.[i] ?? String(v) }));
  return null;
}

function FormCard({ ask, actions }: { ask: Ask; actions: AskActions }) {
  const required = new Set(ask.schema?.required ?? []);
  // The daemon's JSON doesn't keep the server's field order (keys come
  // sorted), so what's required comes first.
  const props = Object.entries(ask.schema?.properties ?? {}).sort(([a], [b]) => Number(required.has(b)) - Number(required.has(a)));
  const [values, setValues] = useState<Record<string, unknown>>(() => {
    const v: Record<string, unknown> = {};
    for (const [k, f] of props) if (f.default !== undefined) v[k] = f.default;
    return v;
  });
  const set = (k: string, v: unknown) => setValues({ ...values, [k]: v });
  const filled = (v: unknown) => v !== undefined && v !== "" && !(Array.isArray(v) && v.length === 0);
  const ready = [...required].every((k) => filled(values[k]) || ask.schema?.properties?.[k]?.type === "boolean");
  const submit = () => {
    const content: Record<string, unknown> = {};
    for (const [k, v] of Object.entries(values)) if (filled(v)) content[k] = v;
    actions.answer(content);
  };
  return (
    <>
      <div class="ask-message">{ask.message}</div>
      {props.map(([key, f]) => {
        const title = f.title ?? key;
        const opts = choices(f);
        const many = f.type === "array" ? choices(f.items) : null;
        const label = (
          <span class="ask-field-title">
            {title}
            {required.has(key) && <span class="ask-required"> *</span>}
          </span>
        );
        const desc = f.description && <span class="ask-desc">{f.description}</span>;
        if (many) {
          const cur = (values[key] as (string | number)[] | undefined) ?? [];
          return (
            <fieldset key={key} class="ask-field" data-field={key}>
              <legend>{label}</legend>
              {desc}
              <div class="ask-options multi" role="group">
                {many.map((c) => (
                  <label key={String(c.value)} class={cur.includes(c.value) ? "ask-option on" : "ask-option"}>
                    <input
                      type="checkbox"
                      checked={cur.includes(c.value)}
                      onChange={() => set(key, cur.includes(c.value) ? cur.filter((x) => x !== c.value) : [...cur, c.value])}
                    />
                    <span class="ask-label">{c.label}</span>
                    {c.description && <span class="ask-desc">{c.description}</span>}
                  </label>
                ))}
              </div>
            </fieldset>
          );
        }
        if (opts) {
          return (
            <fieldset key={key} class="ask-field" data-field={key}>
              <legend>{label}</legend>
              {desc}
              <div class="ask-options" role="radiogroup">
                {opts.map((c) => (
                  <button
                    key={String(c.value)}
                    type="button"
                    role="radio"
                    aria-checked={values[key] === c.value}
                    class={values[key] === c.value ? "ask-option on" : "ask-option"}
                    onClick={() => set(key, values[key] === c.value ? undefined : c.value)}
                  >
                    <span class="ask-label">{c.label}</span>
                    {c.description && <span class="ask-desc">{c.description}</span>}
                  </button>
                ))}
              </div>
            </fieldset>
          );
        }
        if (f.type === "boolean") {
          return (
            <label key={key} class="ask-field ask-check" data-field={key}>
              <input type="checkbox" checked={values[key] === true} onChange={(e) => set(key, (e.currentTarget as HTMLInputElement).checked)} />
              {label}
              {desc}
            </label>
          );
        }
        const value = values[key] === undefined ? "" : String(values[key]);
        return (
          <label key={key} class="ask-field" data-field={key}>
            {label}
            {desc}
            {f.type === "integer" || f.type === "number" ? (
              <input
                type="number"
                name={key}
                min={f.minimum}
                max={f.maximum}
                step={f.type === "integer" ? 1 : undefined}
                value={value}
                onInput={(e) => {
                  const raw = (e.currentTarget as HTMLInputElement).value;
                  set(key, raw === "" ? undefined : Number(raw));
                }}
              />
            ) : f.format === "markdown" || (f.maxLength ?? 0) > 200 ? (
              <textarea name={key} rows={6} value={value} onInput={(e) => set(key, (e.currentTarget as HTMLTextAreaElement).value)} />
            ) : (
              <input type="text" name={key} value={value} onInput={(e) => set(key, (e.currentTarget as HTMLInputElement).value)} />
            )}
          </label>
        );
      })}
      {/* M36: a forge draft is sent (with your login) or dropped. #234: an
          agent's invite is sent, or declined with the reason you gave. */}
      <Buttons
        actions={ask.source === "invite" ? { ...actions, decline: () => actions.answer({ decline: true, ...(filled(values.reason) ? { reason: values.reason } : {}) }) } : actions}
        submit={submit}
        disabled={!ready}
        labels={ask.source === "forge" ? { submit: "Send", decline: "Drop" } : ask.source === "invite" ? { submit: "Invite", decline: "Decline" } : undefined}
      />
    </>
  );
}

// ------------------------------------------------------------ sign-in link

function LinkCard({ ask, actions }: { ask: Ask; actions: AskActions }) {
  return (
    <>
      <div class="ask-message">{ask.message}</div>
      {ask.url && <div class="ask-url">{ask.url}</div>}
      <div class="ask-buttons">
        {ask.accepted ? (
          <span class="ask-waiting">Waiting for it to finish…</span>
        ) : (
          <button
            class="primary"
            onClick={() => {
              if (ask.url) window.open(ask.url, "_blank", "noopener");
              actions.answer({});
            }}
          >
            Open link
          </button>
        )}
        <button onClick={() => actions.decline()}>Dismiss</button>
      </div>
    </>
  );
}
