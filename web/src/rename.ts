// The browser's storage under its names from before the rename (#505).
// main.tsx imports this first, so it runs before anything reads storage:
// each old key whose new key is absent is copied over. Copied, not moved:
// a daemon still on 0.25 serves its older page on the same origin, and
// that page reads the old keys. Drop with the rest of the bridge (#508).

const LOCAL: [string, string][] = [
  ["illogical.hosts", "arugula.hosts"],
  ["illogical.host", "arugula.host"],
  ["illogical.control.host", "arugula.control.host"],
  ["illogical.control.pins", "arugula.control.pins"],
  ["illogical.control.directory", "arugula.control.directory"],
  ["illogical.fleet", "arugula.fleet"],
  ["illogical-hand", "arugula-hand"],
  ["illogical.palette.recent", "arugula.palette.recent"],
  ["illogical.update.dismissed", "arugula.update.dismissed"],
  ["illogical.install-hint", "arugula.install-hint"],
  ["illogical.agents-nudge", "arugula.agents-nudge"],
  ["illogical.getting-started", "arugula.getting-started"],
  ["illogical.agent", "arugula.agent"],
  ["illogical.swarm.by", "arugula.swarm.by"],
  ["illogical.swarm.theme", "arugula.swarm.theme"],
  ["illogical.control-dropped.opened", "arugula.control-dropped.opened"],
];

const SESSION: [string, string][] = [
  ["illogical:presigned-invite", "arugula:presigned-invite"],
  ["illogical.control.recover", "arugula.control.recover"],
  ["illogical.control-dropped.hidden", "arugula.control-dropped.hidden"],
];

function copy(store: () => Storage, keys: [string, string][]) {
  for (const [old, now] of keys) {
    // Private modes can refuse storage altogether, or a write.
    try {
      const s = store();
      const v = s.getItem(old);
      if (v !== null && s.getItem(now) === null) s.setItem(now, v);
    } catch {
      // Nothing to carry over.
    }
  }
}

copy(() => localStorage, LOCAL);
copy(() => sessionStorage, SESSION);
