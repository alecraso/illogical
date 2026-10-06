/** What's built but not offered: the city, hive and timeline views of the
 * swarm, and an ssh invite in a pane's menu. They need things a newcomer
 * doesn't have, so they stay out of the menus unless this browser sets
 * `illogical.more` to 1 (the tests of those features do). */
export function showMore(): boolean {
  try {
    return localStorage.getItem("illogical.more") === "1";
  } catch {
    return false;
  }
}
