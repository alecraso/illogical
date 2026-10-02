// Team rosters (M19): the browser's copy of `illogical_e2e::team`. Owners'
// browsers sign each new version; the same rules as daemons decide which
// follow.

import { evaluate, type Cert, type Revocation } from "./cert.ts";
import { signText, type DeviceKeys } from "./keys.ts";

export type TeamRole = "owner" | "editor" | "viewer";

export interface Member {
  account: string;
  root: string;
  role: TeamRole;
  name: string;
}

export interface Roster {
  v: number;
  team: string;
  name: string;
  version: number;
  at: number;
  members: Member[];
  by: string;
  sig: string;
}

export interface TeamPin {
  team: string;
  founder: string;
  founder_root: string;
}

export type AccountCerts = Record<string, [Cert[], Revocation[]]>;

export function rosterBody(r: Roster): string {
  let b = `illogical team v1\nteam ${r.team}\nname ${r.name}\nversion ${r.version}\nat ${r.at}\n`;
  for (const m of r.members) b += `member ${m.account} ${m.root} ${m.role} ${m.name}\n`;
  return b + `by ${r.by}\n`;
}

/** A name in a roster: no spaces or control characters. */
export const word = (s: string) => s.replace(/[\s\u0000-\u001f\u007f-\u009f]+/g, "-").slice(0, 120) || "someone";

export async function signRoster(r: Omit<Roster, "by" | "sig">, keys: DeviceKeys): Promise<Roster> {
  const out: Roster = { ...r, by: keys.id, sig: "" };
  out.sig = await signText(keys, rosterBody(out));
  return out;
}

async function verify(signHex: string, msg: string, sigHex: string): Promise<boolean> {
  try {
    const un = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
    const key = await crypto.subtle.importKey("raw", un(signHex), { name: "Ed25519" }, false, ["verify"]);
    return await crypto.subtle.verify("Ed25519", key, un(sigHex), new TextEncoder().encode(msg));
  } catch {
    return false;
  }
}

/** Whether `r` may follow `prev` (or start the team, as `pin` says). */
export async function follows(r: Roster, prev: Roster | null, pin: TeamPin, certs: AccountCerts): Promise<boolean> {
  if (r.v !== 1 || r.team !== pin.team || !r.members.some((m) => m.role === "owner")) return false;
  if (prev && r.version <= prev.version) return false;
  const signers: Pick<Member, "account" | "root">[] = prev
    ? prev.members.filter((m) => m.role === "owner")
    : [{ account: pin.founder, root: pin.founder_root }];
  for (const m of signers) {
    const [c, rv] = certs[m.account] ?? [[], []];
    const d = (await evaluate({ account: m.account, root: m.root }, c, rv)).get(r.by);
    if (d && d.kind !== "daemon" && (await verify(d.sign, rosterBody(r), r.sig))) return true;
  }
  return false;
}
