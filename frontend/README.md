# Review Queue UI shell

The React/Vite shell implements the Queue Home and a dark, accessible reviewer workspace from the product specification. It uses local fixture data only and does not request authentication, contact Copilot, or perform queue mutations.

Run it with Node 20+:

```sh
npm install
npm run dev
```

`npm run build` type-checks and produces a static build. The UI is deliberately isolated under `frontend/` so a Tauri command adapter can replace the fixtures without changing the visual components.
