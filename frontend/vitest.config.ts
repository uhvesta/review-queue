import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tests deliberately exercise the same fixture-backed API surface used by
// `npm run dev:fixture`; production Vite configuration remains unchanged.
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [
      {
        find: /^\.\/api$/,
        replacement: fileURLToPath(new URL("./src/api.fixture.ts", import.meta.url)),
      },
    ],
  },
  test: {
    environment: "jsdom",
    setupFiles: ["./test/setup.ts"],
    clearMocks: true,
  },
});
