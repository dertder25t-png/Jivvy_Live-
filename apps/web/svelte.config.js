import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/**
 * A static single-page app: Cloudflare Pages serves it at app.jivvy.org, and the daemon can
 * serve the same files on the church's network with no server code (Stage 1A, later item).
 */
export default {
  preprocess: vitePreprocess(),
  kit: { adapter: adapter({ fallback: "index.html" }) },
};
