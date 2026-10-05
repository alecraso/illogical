// Runs probe/bench.html?auto in Google Chrome (headed, on the desktop's
// display) and in Playwright's WebKit, and prints each result.
//   PW=/path/to/web/node_modules/@playwright/test node bench-browsers.cjs [chrome|webkit]...
const path = require("path");
const { chromium, webkit } = require(process.env.PW || path.join(__dirname, "../../web/node_modules/@playwright/test"));
const url = "file://" + path.join(__dirname, "probe/bench.html") + "?auto";
const engines = {
  chrome: () => chromium.launch({ channel: "chrome", headless: false, args: ["--window-size=1240,800", "--ozone-platform=wayland"] }),
  webkit: () => webkit.launch({ headless: false }),
};
(async () => {
  for (const name of process.argv.slice(2).length ? process.argv.slice(2) : ["chrome"]) {
    const browser = await engines[name]();
    const page = await browser.newPage({ viewport: { width: 1220, height: 780 } });
    await page.goto(url);
    const result = await page.waitForFunction(() => window.__s25result, null, { timeout: 120000, polling: 500 });
    console.log(`${name} ${JSON.stringify(await result.jsonValue())}`);
    await browser.close();
  }
})();
