# illogical-control-wire

The JSON that daemons and illogical control send each other to enrol, to learn
their account's and team's certificates, to say who gets in, and to dial the
relay: one type per message, so a renamed field breaks both builds. The routes
are `pub const`s here too. Push, TURN, sandbox and forge messages aren't here
yet (#491, #450).

Depends on `illogical-e2e` (certificates, rosters) and serde. Not on
`illogical-proto`: control shouldn't pull in the client wire types.

Start with `src/lib.rs`: its header says what must stay true of every type
(the bytes on the wire don't change; deployed daemons and control upgrade
separately). `src/join.rs` is enrolment, `src/team.rs` the certificates.
