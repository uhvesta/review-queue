# Review Queue UI shell

The React/Vite shell implements Queue Home and a dark, compact difit-derived reviewer workspace from the product specification. All authentication, Copilot contact, and queue mutations happen through the Tauri command boundary in `src/api.ts` — the UI never bypasses it, contains no credentials, and issues no network requests directly. Every mutating action still requires its normal explicit user gesture (Submit, Send, Publish, Approve, and so on); nothing fires on load, reopen, or restart.

Outside a Tauri window (e.g. `npm run dev` in a browser) `desktopAvailable` is `false` and the app falls back to a read-only standalone informational state instead of a fixture-populated UI, since there is no Tauri runtime to serve real queue/round data. To see the app fully populated, run the desktop shell (`src-tauri/`) rather than the Vite dev server alone.

Run the frontend alone with Node 20+ (useful for layout/style iteration, not for exercising real data):

```sh
npm install
npm run dev
```

`npm run build` type-checks and produces a static build. The UI is isolated under `frontend/` so it can be iterated on independently, but it is not standalone in the sense of having its own data layer — `src/api.ts` is the single, closed set of calls into the Rust/Tauri backend.
