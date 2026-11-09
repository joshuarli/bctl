import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { createServer, type Server } from "node:http";
import { resolve } from "node:path";
import { expect, test, type Page } from "@playwright/test";

const workspace = resolve(import.meta.dirname, "../..");
const image = process.env.BCTL_TEST_IMAGE ?? "ref-alpine-chromium";
const bctlBinary = resolve(workspace, "target/debug/bctl");
const home = resolve(workspace, `.work/tmp/bctl-d2-${process.pid}`);
const session = `d2-${process.pid}`;
const hostRange = "100.96.0.0/30";
const hostFromGuest = "100.96.0.1";
const grab = process.env.BCTL_GRAB;

function bctl(...args: string[]): string {
  return execFileSync(bctlBinary, args, { env: { ...process.env, BCTL_HOME: home }, encoding: "utf8" });
}

const filler = `<p>${"Fixture text so the page clears the minimum-content check. ".repeat(30)}</p>`;

function loginSite(url: URL, signedIn: boolean): { status: number; headers: Record<string, string>; body: string } {
  const html = (body: string) => ({ status: 200, headers: { "Content-Type": "text/html; charset=utf-8" }, body });
  if (url.pathname === "/login" && url.searchParams.get("user") === "operator") {
    return { status: 302, headers: { "Set-Cookie": "auth=ok; Path=/", Location: "/" }, body: "" };
  }
  switch (url.pathname) {
    case "/style.css":
      return { status: 200, headers: { "Content-Type": "text/css" }, body: "body { color: #123; }" };
    case "/login":
    case "/":
    case "/a.html":
    case "/b.html":
      if (signedIn && url.pathname !== "/login") {
        return html(`<!doctype html><html><head><title>Members ${url.pathname}</title><link rel=stylesheet href=/style.css></head><body><h1>members ${url.pathname}</h1><a href=/a.html>a</a> <a href=/b.html>b</a>${filler}</body></html>`);
      }
      return html(`<!doctype html><html><head><title>Sign in</title></head><body><form action=/login><input id=user name=user><input id=pass name=pass type=password><div class="g-recaptcha" data-sitekey=fixture></div><button>Sign in</button></form>${filler}</body></html>`);
    default:
      return { status: 404, headers: {}, body: "not found" };
  }
}

let site: Server;
let sitePort: number;
let loginHits: string[] = [];
let origin: string;
let runId: number;
let outputDir: string;

test.describe.configure({ mode: "serial" });

test.beforeAll(async () => {
  if (!existsSync(bctlBinary)) throw new Error(`build bctl first: cargo build -p bctl (${bctlBinary})`);
  if (!grab || !existsSync(grab)) throw new Error("set BCTL_GRAB to Shadowdriver's grab executable (grab/.venv/bin/grab)");
  site = createServer((request, response) => {
    const url = new URL(request.url ?? "/", "http://fixture");
    loginHits.push(url.pathname);
    const reply = loginSite(url, (request.headers.cookie ?? "").includes("auth=ok"));
    response.writeHead(reply.status, { "Content-Length": Buffer.byteLength(reply.body), Connection: "close", ...reply.headers });
    response.end(reply.body);
  });
  await new Promise<void>((done) => site.listen(0, "127.0.0.1", done));
  const address = site.address();
  if (typeof address !== "object" || address === null) throw new Error("fixture has no port");
  sitePort = address.port;
  mkdirSync(resolve(home, "runtime"), { recursive: true });
  mkdirSync(resolve(home, "images"), { recursive: true });
  symlinkSync(resolve(workspace, ".work/runtime/smolvm"), resolve(home, "runtime/smolvm"));
  symlinkSync(resolve(workspace, `.work/images/${image}.tar`), resolve(home, `images/${image}.tar`));
  origin = new URL(bctl("serve").trim()).origin;
  bctl("up", session, "--profile", session, "--image", image, "--allow-private", hostRange);
});

test.afterAll(async () => {
  site?.close();
  try { bctl("down", session); } catch {}
  try {
    const pid = execFileSync("lsof", ["-t", resolve(home, "daemon.lock")], { encoding: "utf8" }).trim();
    if (pid) process.kill(Number(pid));
  } catch {}
  rmSync(home, { recursive: true, force: true });
});

async function signIn(page: Page): Promise<void> {
  await page.goto(bctl("serve").trim());
}

async function typeAddress(page: Page, address: string): Promise<void> {
  for (const char of address) {
    if (char === ":") await page.keyboard.press("Shift+Semicolon");
    else await page.keyboard.type(char);
  }
}

async function canvasPoint(page: Page, x: number, y: number): Promise<void> {
  await page.getByTestId("viewer").locator("canvas").click({ position: { x, y } });
}

test("a crawl behind a login is taken over, logged in by hand, and resumed from the dashboard", async ({ page }) => {
  const seed = `http://${hostFromGuest}:${sitePort}/`;
  runId = (JSON.parse(bctl("grab", seed, "--session", session, "--json")) as { runs: { id: number }[] }).runs[0]!.id;
  await signIn(page);
  await page.goto(`${origin}/runs/${runId}`);

  await expect(page.getByRole("heading", { name: `Run #${runId}` })).toBeVisible();
  await expect(page.getByText("needs-interaction").first()).toBeVisible({ timeout: 240_000 });
  await expect(page.getByText(`The run needs you: challenge at ${seed}`)).toBeVisible();
  expect(loginHits).not.toContain("/a.html");
  await expect(page.getByTestId("authority")).toHaveText("Automation holds control and the run waits for you.");

  await expect(page.getByTestId("stream")).toHaveText("live", { timeout: 30_000 });
  const canvas = page.getByTestId("viewer").locator("canvas");
  await page.getByRole("button", { name: "Take control" }).click();
  await expect(page.getByTestId("authority")).toContainText("You have control");

  const seedHits = () => loginHits.filter((path) => path === "/").length;
  const before = seedHits();
  await canvasPoint(page, 300, 42);
  await page.waitForTimeout(500);
  await page.keyboard.press("Control+A");
  await typeAddress(page, seed);
  await page.keyboard.press("Enter");
  await expect.poll(seedHits, { timeout: 30_000 }).toBeGreaterThan(before);
  await page.waitForTimeout(2000);
  await canvasPoint(page, 300, 300);
  await page.keyboard.press("Tab");
  await page.keyboard.type("operator");
  await page.keyboard.press("Tab");
  await page.keyboard.type("secret");
  await page.keyboard.press("Enter");
  await expect.poll(() => loginHits.includes("/login"), { timeout: 30_000 }).toBe(true);

  await page.getByRole("button", { name: "Resume automation" }).click();
  await expect(page.getByTestId("authority")).not.toContainText("You have control");
  await expect(page.getByTestId("outcome")).toHaveText("succeeded", { timeout: 300_000 });
  await expect(page.getByRole("heading", { name: `Run #${runId}` })).toBeVisible();
  await expect(page.getByTestId("log")).not.toHaveText("The log is empty.");
  await expect(page.getByTestId("progress")).toContainText("pages done");
  await expect(page.getByTestId("artifacts")).toContainText("index.html");

  const record = JSON.parse(bctl("runs", String(runId), "--json")) as { runs: { state: string; detail: string; output_dir: string; attempts: unknown[] }[] };
  const done = record.runs[0]!;
  expect([done.state, done.detail, done.attempts.length]).toEqual(["succeeded", "verified 1 mirror(s)", 1]);
  outputDir = done.output_dir;
  const mirror = resolve(outputDir, hostFromGuest);
  expect(readFileSync(resolve(mirror, "index.html"), "utf8")).toContain("members /");
  expect(readFileSync(resolve(mirror, "a.html/index.html"), "utf8")).toContain("members /a.html");
  expect(readFileSync(resolve(mirror, "b.html/index.html"), "utf8")).toContain("members /b.html");
});

test("captured html is only ever a download, and run endpoints need the session and the origin", async ({ page, playwright }) => {
  writeFileSync(resolve(outputDir, "evil.html"), "<!doctype html><title>pwned</title><script>document.title='executed'</script>");
  await signIn(page);
  await page.goto(`${origin}/runs/${runId}`);
  const link = page.getByTestId("artifacts").getByRole("link", { name: "evil.html" });
  await expect(link).toBeVisible({ timeout: 15_000 });

  const [download] = await Promise.all([page.waitForEvent("download"), link.click()]);
  expect(download.suggestedFilename()).toBe("evil.html");
  await expect(page).toHaveURL(`${origin}/runs/${runId}`);

  const served = await page.request.get(`${origin}/api/runs/${runId}/artifacts/evil.html`);
  expect(served.status()).toBe(200);
  expect(served.headers()["content-disposition"]).toMatch(/^attachment;/);
  expect(served.headers()["content-type"]).toBe("application/octet-stream");
  expect(served.headers()["content-security-policy"]).toContain("sandbox");
  expect(served.headers()["x-content-type-options"]).toBe("nosniff");
  expect((await page.request.get(`${origin}/api/runs/${runId}/artifacts/..%2F..%2Fstate.db`)).status()).toBe(404);

  const stranger = await playwright.request.newContext();
  expect((await stranger.get(`${origin}/api/runs/${runId}`)).status()).toBe(401);
  expect((await stranger.get(`${origin}/api/runs/${runId}/artifacts/evil.html`)).status()).toBe(401);
  expect((await page.request.post(`${origin}/api/runs/${runId}/resume`, { headers: { Origin: "http://evil.example" } })).status()).toBe(403);
  expect((await page.request.post(`${origin}/api/runs/${runId}/resume`)).status()).toBe(403);
  await stranger.dispose();
});
