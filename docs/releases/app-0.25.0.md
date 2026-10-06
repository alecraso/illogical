# illogical app 0.25.0

The bridge to Arugula (#504), for the app. Update with *Check for
Updates…* before the renamed release comes out.

- The Daemon menu finds, starts, stops and restarts an `arugulad`
  service as well as an `illogicald` one. Without this, an app on a
  machine whose daemon had already updated could register a second
  daemon.
- `ARUGULA_*` variables work wherever `ILLOGICAL_*` ones do.

Not notarized yet (#177): from a browser download, open it once with
*Open Anyway* in System Settings › Privacy & Security.
