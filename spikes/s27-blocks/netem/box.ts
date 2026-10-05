// The box of netem.sh: the test site on loopback and the spike's daemon
// dialed out to control's relay. Prints one JSON line (the daemon's id and
// key, and the site's port), then runs until killed.
import { startS27, testSite } from "../tests/servers.ts";

const site = await testSite();
const { info } = await startS27(["daemon", "--relay", process.argv[2], "--admin", "0.0.0.0:7771", "--direct", "0.0.0.0:7772", "--dir", ".run/cert"]);
console.log(JSON.stringify({ id: info.id, noise: info.noise, site: site.port }));
