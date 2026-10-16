import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [sveltekit()],
  // The daemon only accepts browsers from origins it was given (--ws-origin), so keep one port.
  server: { port: 5173, strictPort: true },
  test: { include: ["src/**/*.test.ts"] },
});
