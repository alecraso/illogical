// The fakes from fakes.ts as one long-running process, for the test
// stack's `control` profile (testnet/compose.yaml): GitHub on 9001, Stripe
// on 9002 and a Web Push endpoint on 9003, on every interface.
//   node --experimental-strip-types fakes-serve.ts

import { fakeGithub, fakePush, fakeStripe } from "./fakes.ts";

const host = "0.0.0.0";
fakeGithub(9001, host);
fakeStripe(9002, host);
fakePush(9003, host);
console.log("fakes: GitHub on 9001, Stripe on 9002, push on 9003");
for (const s of ["SIGTERM", "SIGINT"] as const) process.on(s, () => process.exit(0));
