import { resolve } from "node:path";
import { expect, test } from "@playwright/test";
import { createServer, type ViteDevServer } from "vite";
import type { SessionView } from "../src/api.ts";

let server: ViteDevServer;
let origin: string;

test.beforeAll(async () => {
  const root = resolve(import.meta.dirname, "..");
  server = await createServer({ root, configFile: resolve(root, "vite.config.ts"), server: { host: "127.0.0.1", port: 0 } });
  await server.listen();
  const url = server.resolvedUrls?.local[0];
  if (!url) throw new Error("dashboard fixture has no listening URL");
  origin = url;
});

test.afterAll(async () => { await server?.close(); });

test("paused sessions show a warning badge in the sessions list", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  const session: SessionView = {
    name: "paused-browser", profile: "default", image: "helium",
    resources: { width: 1280, height: 720, cpus: 2, memory_mib: 1024 },
    allow_private: [], state: "paused", generation: 1, stage: null,
    failure: null, machine: null, vnc_port: null, input: null
  };
  await page.route("**/api/sessions", (route) => route.fulfill({ json: { sessions: [session] } }));
  await page.goto(origin);
  const row = page.getByRole("row").filter({ has: page.getByRole("link", { name: session.name }) });
  const badge = row.getByText("paused", { exact: true });
  await expect(badge).toBeVisible();
  await expect(badge).toHaveClass(/\bui-badge-warn\b/);
  expect(errors).toEqual([]);
});
