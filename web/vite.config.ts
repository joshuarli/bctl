import solid from "@solidjs/vite-plugin";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [solid()],
  build: { assetsInlineLimit: 0, sourcemap: false }
});
