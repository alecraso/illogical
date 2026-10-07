// Role words (#101): one set wherever a role is shown or picked (Teams,
// invites, admitting someone, Share).

import type { Role } from "../proto";

const LABELS: Record<Role, string> = { viewer: "watches", editor: "drives", owner: "owner" };

/** "watches", "drives" or "owner". */
export function roleLabel(role: Role): string {
  return LABELS[role] ?? role;
}

/** In a sentence: "as someone who drives", "as an owner". */
export function roleAs(role: Role): string {
  return role === "owner" ? "as an owner" : `as someone who ${roleLabel(role)}`;
}

/** What each role may do in a team, for the Teams panel. */
export const ROLE_HELP: [Role, string][] = [
  ["viewer", "sees the team's machines and panes"],
  ["editor", "also types, answers agents and sends follow-ups"],
  ["owner", "also invites people, changes roles, takes machines out and locks the team"],
];
