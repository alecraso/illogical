// Something to move somewhere else (#99): a command, a link, a code.

import { useRef, useState } from "preact/hooks";

/**
 * `<CopyText text="arugulad join …" />`: the value in mono with a Copy
 * button beside it that says "Copied" for a moment. Where the clipboard is
 * blocked (plain http, a refused permission) it selects the value instead
 * and says "Selected", so the person copies it themselves.
 *
 * - `text`: what's shown and copied.
 * - `inline`: a short value inside a sentence (an id), not a block on its
 *   own line (a command, a link).
 * - `share`: also a Share button where the browser has navigator.share
 *   (phones), for links meant for someone else.
 * - Anything else (`data-*` attributes) lands on the value's element, so
 *   tests find the value without the buttons' text.
 */
export function CopyText({ text, inline, share, ...attrs }: { text: string; inline?: boolean; share?: boolean; [data: `data-${string}`]: unknown }) {
  const value = useRef<HTMLElement>(null);
  return (
    <span class={inline ? "copy-text inline" : "copy-text"}>
      <span ref={value} class={inline ? "control-cmd-inline" : "control-cmd"} {...attrs}>
        {text}
      </span>
      <CopyButton text={text} select={() => value.current} />
      {share && typeof navigator.share === "function" ? (
        <button type="button" class="copy-button" data-share onClick={() => void navigator.share({ url: text }).catch(() => {})}>
          Share
        </button>
      ) : null}
    </span>
  );
}

/** Just the button, for something shown another way (recovery codes' "Copy both"). */
export function CopyButton({ text, label = "Copy", select }: { text: string; label?: string; select?: () => HTMLElement | null }) {
  const [said, setSaid] = useState("");
  const say = (s: string) => {
    setSaid(s);
    setTimeout(() => setSaid(""), 1600);
  };
  return (
    <button
      type="button"
      class="copy-button"
      data-copy
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          return say("Copied");
        } catch {
          // Blocked: select it, and try the old way while it's selected.
        }
        const el = select?.();
        if (!el) return say("Couldn't copy");
        const range = document.createRange();
        range.selectNodeContents(el);
        getSelection()?.removeAllRanges();
        getSelection()?.addRange(range);
        let ok = false;
        try {
          ok = document.execCommand("copy");
        } catch {
          // stays selected
        }
        say(ok ? "Copied" : "Selected");
      }}
    >
      {said || label}
    </button>
  );
}

/** Save text as a file (recovery codes' "Download .txt"). */
export function download(name: string, text: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
}
