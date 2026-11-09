import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, symlinkSync } from "node:fs";
import { request as httpRequest } from "node:http";
import { resolve } from "node:path";
import { chromium, expect, test, type Browser, type Page } from "@playwright/test";

const workspace = resolve(import.meta.dirname, "../..");
const bctlBinary = resolve(workspace, "target/debug/bctl");
const home = resolve(workspace, `.work/tmp/bctl-d1-${process.pid}`);
const session = `d1${process.pid}`;

function bctl(...args: string[]): string {
  return execFileSync(bctlBinary, args, { env: { ...process.env, BCTL_HOME: home }, encoding: "utf8" });
}

type Session = { name: string; state: string };

let guest: Browser;
let guestPage: Page;
let loginUrl: string;
let origin: string;

test.beforeAll(async () => {
  if (!existsSync(bctlBinary)) throw new Error(`build bctl first: cargo build -p bctl (${bctlBinary})`);
  mkdirSync(resolve(home, "runtime"), { recursive: true });
  mkdirSync(resolve(home, "images"), { recursive: true });
  symlinkSync(resolve(workspace, ".work/runtime/smolvm"), resolve(home, "runtime/smolvm"));
  symlinkSync(resolve(workspace, ".work/images/ref-alpine-chromium.tar"), resolve(home, "images/ref-alpine-chromium.tar"));
  bctl("up", session, "--image", "ref-alpine-chromium");
  guest = await chromium.connectOverCDP(bctl("cdp-url", session).trim());
  const context = guest.contexts()[0];
  if (!context) throw new Error("guest browser has no context");
  guestPage = context.pages()[0] ?? (await context.newPage());
  loginUrl = bctl("serve").trim();
  origin = new URL(loginUrl).origin;
});

test.afterAll(async () => {
  await guest?.close().catch(() => {});
  try { bctl("down", session); } catch {}
  try {
    const pid = execFileSync("lsof", ["-t", resolve(home, "daemon.lock")], { encoding: "utf8" }).trim();
    if (pid) process.kill(Number(pid));
  } catch {}
  rmSync(home, { recursive: true, force: true });
});

function rawUpgrade(path: string, headers: Record<string, string>): Promise<number> {
  return new Promise((done, fail) => {
    const url = new URL(path, origin);
    const req = httpRequest({
      host: url.hostname, port: url.port, path: url.pathname, method: "GET",
      headers: { Connection: "Upgrade", Upgrade: "websocket", "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ==", ...headers }
    });
    req.on("upgrade", (res, socket) => { socket.destroy(); done(res.statusCode ?? 0); });
    req.on("response", (res) => { res.resume(); done(res.statusCode ?? 0); });
    req.on("error", fail);
    req.end();
  });
}

async function viewerShot(page: Page): Promise<Buffer> {
  return page.getByTestId("viewer").locator("canvas").screenshot();
}

test("the stream is refused without a dashboard session or from another origin", async ({ page }) => {
  expect(await rawUpgrade(`/api/sessions/${session}/vnc`, { Origin: origin })).toBe(401);
  await page.goto(loginUrl);
  const cookie = (await page.context().cookies()).find((c) => c.name === "bctl_session");
  expect(cookie?.httpOnly).toBe(true);
  expect(cookie?.sameSite).toBe("Strict");
  const header = `bctl_session=${cookie?.value}`;
  expect(await rawUpgrade(`/api/sessions/${session}/vnc`, { Origin: "http://evil.example", Cookie: header })).toBe(403);
  expect(await rawUpgrade(`/api/sessions/${session}/vnc`, { Origin: origin, Cookie: header })).toBe(101);
  const reused = await page.request.get(loginUrl, { maxRedirects: 0 });
  expect(reused.status()).toBe(401);
});

test("the live view follows the browser and carries typing into the page", async ({ page }) => {
  await page.goto(bctl("serve").trim());
  await page.getByRole("link", { name: session }).click();
  await expect(page.getByTestId("stream")).toHaveText("live", { timeout: 30_000 });

  await guestPage.goto("data:text/html,<body style='margin:0;background:%23fff'><textarea id=t autofocus style='width:100vw;height:100vh;border:0;font:40px sans-serif'></textarea></body>");
  await expect.poll(async () => (await viewerShot(page)).length).toBeGreaterThan(0);
  const before = await viewerShot(page);
  await guestPage.evaluate(() => { document.body.style.background = "#c03"; (document.getElementById("t") as HTMLTextAreaElement).style.background = "#c03"; });
  await expect.poll(async () => Buffer.compare(await viewerShot(page), before) !== 0, { timeout: 10_000 }).toBe(true);

  await page.getByRole("button", { name: "Take control" }).click();
  await expect(page.getByRole("button", { name: "Release control" })).toBeVisible();
  await page.getByTestId("viewer").locator("canvas").click({ position: { x: 200, y: 200 } });
  await page.keyboard.type("hello dashboard");
  await expect.poll(() => guestPage.evaluate(() => (document.getElementById("t") as HTMLTextAreaElement).value), { timeout: 10_000 }).toBe("hello dashboard");

  await page.reload();
  await expect(page.getByTestId("stream")).toHaveText("live", { timeout: 30_000 });
});

test("a login link can open a session directly but never redirects off the dashboard", async ({ page }) => {
  await page.goto(`${bctl("serve").trim()}&next=/sessions/${session}`);
  await expect(page).toHaveURL(`${origin}/sessions/${session}`);
  await page.context().clearCookies();
  await page.goto(`${bctl("serve").trim()}&next=//evil.example/`);
  await expect(page).toHaveURL(`${origin}/`);
});

async function rawKeys(page: Page, name: string, text: string): Promise<void> {
  await page.evaluate(async ({ name, text }) => {
    const ws = new WebSocket(`ws://${location.host}/api/sessions/${name}/vnc`);
    ws.binaryType = "arraybuffer";
    let received = 0;
    await new Promise<void>((done, fail) => {
      ws.onerror = () => fail(new Error("websocket failed"));
      ws.onmessage = (event) => {
        received += (event.data as ArrayBuffer).byteLength;
        if (received >= 12 && ws.readyState === WebSocket.OPEN && !(ws as unknown as { sent?: boolean }).sent) {
          (ws as unknown as { sent?: boolean }).sent = true;
          ws.send(new TextEncoder().encode("RFB 003.008\n"));
          ws.send(new Uint8Array([1]));
          ws.send(new Uint8Array([1]));
          for (const char of text) {
            const sym = char.charCodeAt(0);
            for (const down of [1, 0]) ws.send(new Uint8Array([4, down, 0, 0, 0, 0, (sym >> 8) & 255, sym & 255]));
          }
          setTimeout(() => { ws.close(); done(); }, 500);
        }
      };
    });
  }, { name, text });
}

test("only the viewer holding control can send input", async ({ browser }) => {
  const holder = await (await browser.newContext()).newPage();
  await holder.goto(`${bctl("serve").trim()}&next=/sessions/${session}`);
  await expect(holder.getByTestId("stream")).toHaveText("live", { timeout: 30_000 });
  await guestPage.goto("data:text/html,<textarea id=t autofocus style='width:100vw;height:100vh'></textarea>");
  await holder.getByRole("button", { name: "Take control" }).click();
  await expect(holder.getByRole("button", { name: "Release control" })).toBeVisible();

  const watcher = await (await browser.newContext()).newPage();
  await watcher.goto(`${bctl("serve").trim()}&next=/sessions/${session}`);
  await expect(watcher.getByText("Another viewer has control")).toBeVisible({ timeout: 10_000 });
  await expect(watcher.getByRole("button", { name: "Take control" })).toBeVisible();
  const refused = await watcher.evaluate(async (name) => (await fetch(`/api/sessions/${name}/control`, { method: "POST" })).status, session);
  expect(refused).toBe(409);

  await guestPage.bringToFront();
  await guestPage.focus("#t");
  await rawKeys(watcher, session, "zz");
  await rawKeys(holder, session, "ok");
  await expect.poll(() => guestPage.evaluate(() => (document.getElementById("t") as HTMLTextAreaElement).value), { timeout: 10_000 }).toBe("ok");

  await holder.getByRole("button", { name: "Release control" }).click();
  await expect(watcher.getByRole("button", { name: "Take control" })).toBeVisible();
  const taken = await watcher.evaluate(async (name) => (await fetch(`/api/sessions/${name}/control`, { method: "POST" })).status, session);
  expect(taken).toBe(200);
});
