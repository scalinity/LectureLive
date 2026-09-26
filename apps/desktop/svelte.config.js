// Tauri doesn't have a Node.js server to do proper SSR
// so we use adapter-static with a fallback to index.html to put the site in SPA mode
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/ for more info
import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({
      fallback: "index.html",
    }),
    // Spec §9.3: nothing the page renders can load from the network. The dev server sends this as a
    // header with a nonce on SvelteKit's own script; a built page carries it as a meta tag, and
    // tauri.conf.json gives the custom protocol the same directives. Styles stay inline-capable
    // because Svelte and Vite inject them; scripts never are.
    csp: {
      mode: "auto",
      directives: {
        "default-src": ["self"],
        "script-src": ["self"],
        "style-src": ["self", "unsafe-inline"],
        "img-src": ["self", "asset:", "http://asset.localhost"],
        "connect-src": ["self", "ipc:", "http://ipc.localhost", "ws://localhost:1420", "ws://127.0.0.1:1420"],
        "font-src": ["self"],
        "object-src": ["none"],
        "frame-src": ["none"],
        "base-uri": ["none"],
        "form-action": ["none"],
      },
    },
  },
};

export default config;
