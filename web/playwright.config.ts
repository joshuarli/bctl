import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "tests",
  timeout: 180_000,
  workers: 1,
  use: { viewport: { width: 1400, height: 1000 } }
});
