import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The `fixture` mode (used only by `npm run dev:fixture`) redirects the `./api` specifier
// imported by App.tsx to the dev-only static fixture backend in `src/api.fixture.ts`, so the
// UI can be visually QA'd in a plain browser without a working Tauri backend. Every other mode
// (including the default `npm run dev` and the production `npm run build`) leaves the alias
// list empty, so `src/api.ts` — the real Tauri command boundary — is always what ships.
export default defineConfig(({ mode }) => ({
  plugins: [react()],
  resolve: {
    alias:
      mode === "fixture"
        ? [
            {
              find: /^\.\/api$/,
              replacement: fileURLToPath(new URL("./src/api.fixture.ts", import.meta.url)),
            },
          ]
        : [],
  },
}));
