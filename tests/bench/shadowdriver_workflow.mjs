import assert from "node:assert/strict";
import { writeFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const { Browser, BrowserIdentityMode } = await import(pathToFileURL(process.env.SHADOWDRIVER_MODULE));
const browser = await Browser.connect({
  control_url: process.env.BCTL_CDP_URL,
  profile_dir: process.env.WORKFLOW_PROFILE,
  headless: true,
  identity_mode: BrowserIdentityMode.BROWSER_NATIVE,
  stealth: false,
});
try {
  const tab = await browser.new_tab(undefined, false);
  try {
    await tab.command("Emulation.setDeviceMetricsOverride", { width: 1280, height: 720, deviceScaleFactor: 1, mobile: false });
    await tab.navigate(process.env.WORKFLOW_SITE + "/form", "complete");
    const tree = await tab.command("Accessibility.getFullAXTree");
    assert(tree.nodes.some(node => node.role?.value === "textbox" && node.name?.value === "Name"));
    await tab.evaluate("document.querySelector('#name').focus()");
    await tab.command("Input.insertText", { text: "Ada Lovelace" });
    const point = await tab.evaluate("(() => { const r = document.querySelector('#submit').getBoundingClientRect(); return { x: r.x+r.width/2, y:r.y+r.height/2 }; })()");
    for (const type of ["mousePressed", "mouseReleased"]) {
      await tab.command("Input.dispatchMouseEvent", { type, ...point, button: "left", clickCount: 1 });
    }
    const deadline = performance.now() + 10000;
    while (await tab.evaluate("document.querySelector('#out').textContent") !== "sent Ada Lovelace") {
      assert(performance.now() < deadline, "form submission did not complete");
      await new Promise(resolve => setTimeout(resolve, 10));
    }
    const shot = await tab.command("Page.captureScreenshot", { format: "png" });
    const bytes = Buffer.from(shot.data, "base64");
    assert.equal(bytes.subarray(0, 8).toString("hex"), "89504e470d0a1a0a");
    assert.equal(bytes.readUInt32BE(16), 1280);
    assert.equal(bytes.readUInt32BE(20), 720);
    await writeFile(process.env.WORKFLOW_SCREENSHOT, bytes);
    console.log(JSON.stringify({ submitted: "Ada Lovelace", ax_textbox: true, screenshot_bytes: bytes.length, product: (await browser.command("Browser.getVersion")).product }));
  } finally {
    await tab.close();
  }
} finally {
  await browser.close(false);
}
